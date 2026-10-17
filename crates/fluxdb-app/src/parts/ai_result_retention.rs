// 临时快照的磁盘清理与模型引用寿命分离：执行结束即不可读，文件最多保留 24 小时。
fn purge_expired_result_files(storage: &fluxdb_storage::FileStorage) -> fluxdb_core::Result<()> {
    let rows = with_ai_storage(storage, |conn| {
        conn.execute(
            "UPDATE ai_results SET status='expired'
             WHERE retention_kind='retained' AND status='available' AND expires_at_ms<=?1
             AND json_extract(metadata_json,'$.scope')='turn'",
            [ai_chat_now()],
        )
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
        let mut statement = conn
            .prepare(
                "SELECT id,relative_path FROM ai_results
             WHERE retention_kind='retained' AND status='expired' AND expires_at_ms<=?1
             AND json_extract(metadata_json,'$.scope')='turn'
             AND coalesce(json_extract(metadata_json,'$.file_purged'),0)=0
             ORDER BY expires_at_ms,id LIMIT 128",
            )
            .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
        let rows = statement
            .query_map([ai_chat_now()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
    })?;
    let root = storage.root_dir().join("ai-results");
    for (id, relative) in rows {
        if !is_controlled_result_file(&relative) {
            tracing::warn!(result_id=%id, "拒绝清理非法结果相对路径");
            continue;
        }
        if !remove_controlled_file(&root, &relative, "过期结果") {
            continue;
        }
        with_ai_storage(storage, |conn| {
            conn.execute(
                "UPDATE ai_results SET metadata_json=json_set(metadata_json,'$.file_purged',1) WHERE id=?1",
                [&id],
            ).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
            Ok(())
        })?;
    }
    Ok(())
}

// T050：删除已归档会话后的文件清理（spill 结果与产物快照）。
//
// 契约：数据库删除已提交后再删文件；文件删除失败只记录告警、不改变已提交的删库结果。
// 路径严格限制在受控根内：结果必须位于 `ai-results/results/<uuid>.json`，
// 产物必须位于 `ai-artifacts/<uuid>.json`，其余一律拒绝并告警（不越界删除任意文件）。
fn purge_session_files(
    storage: &fluxdb_storage::FileStorage,
    plan: &fluxdb_storage::ai_platform::sessions::SessionPurgePlan,
) {
    let root = storage.root_dir();
    let results_root = root.join("ai-results");
    for relative in &plan.result_files {
        if !is_controlled_result_file(relative) {
            tracing::warn!(%relative, "拒绝清理非法会话结果相对路径");
            continue;
        }
        remove_controlled_file(&results_root, relative, "会话结果");
    }
    let artifacts_root = root.join("ai-artifacts");
    for relative in &plan.artifact_files {
        if !is_controlled_artifact_file(relative) {
            tracing::warn!(%relative, "拒绝清理非法会话产物相对路径");
            continue;
        }
        remove_controlled_file(&artifacts_root, relative, "会话产物");
    }
}

/// 删除受控根内的单个文件；根目录若是符号链接则拒绝，缺失视为已删除。
///
/// 返回文件是否已不在磁盘上（删除成功或本来就缺失）：`false` 表示受控根不可用、
/// 目标越界或删除失败，调用方据此决定是否推进"已清理"状态。
fn remove_controlled_file(root: &std::path::Path, relative: &str, label: &str) -> bool {
    if !root.exists() {
        return true;
    }
    let controlled = match std::fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%relative, %error, label, "受控根不可用，跳过文件清理");
            return false;
        }
    };
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        match std::fs::canonicalize(parent) {
            Ok(parent) if parent.starts_with(&controlled) => {}
            Ok(_) => {
                tracing::warn!(%relative, label, "目标目录越过受控根，拒绝清理");
                return false;
            }
            Err(_) => return false,
        }
    }
    match std::fs::remove_file(&target) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(%relative, %error, label, "文件清理失败，留待重试");
            false
        }
    }
}

// T050：受控存储清理（失败产物快照 + 孤儿文件）与用量统计。
//
// 契约：
// - 只删可判定“无引用”的文件：失败产物落点（`status='failed'` 且已登记路径）与
//   DB 未登记的受控目录内孤儿 `<uuid>.json`。`ready` 产物（供报告读取）一律保留。
// - 文件删除成功后才清空对应 DB 落点；删除失败保留落点，下次重试，不制造悬挂引用。
// - 孤儿清理只扫描受控根目录下的直接子文件，名必须是 `<uuid>.json`，绝不递归/越界。

/// 受控存储用量（产物快照 + spill 结果），供设置页展示与运维核对。
fn ai_storage_usage(
    storage: &fluxdb_storage::FileStorage,
) -> fluxdb_core::Result<fluxdb_storage::ai_platform::artifacts::ArtifactStorageUsage> {
    with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::artifacts::storage_usage(conn)
    })
}

/// 清理失败产物快照：删除文件后清空落点；文件缺失也清空落点（已无占用）。
fn purge_failed_artifact_files(storage: &fluxdb_storage::FileStorage) -> fluxdb_core::Result<usize> {
    let rows = with_ai_storage(storage, |conn| {
        fluxdb_storage::ai_platform::artifacts::failed_artifact_paths(conn)
    })?;
    let root = storage.root_dir().join("ai-artifacts");
    let mut purged = 0usize;
    for (id, relative) in rows {
        if !is_controlled_artifact_file(&relative) {
            tracing::warn!(artifact_id=%id, %relative, "拒绝清理非法产物相对路径");
            continue;
        }
        let target = root.join(&relative);
        let removed = match std::fs::remove_file(&target) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                tracing::warn!(artifact_id=%id, %error, "失败产物文件清理失败，留待重试");
                false
            }
        };
        if removed {
            with_ai_storage(storage, |conn| {
                fluxdb_storage::ai_platform::artifacts::clear_artifact_path(conn, ai_chat_now(), &id)
            })?;
            purged += 1;
        }
    }
    Ok(purged)
}

/// 清理 DB 未登记的孤儿文件：受控根下直接子文件、名形如 `<uuid>.json` 且不在登记集合内。
fn purge_orphan_controlled_files(storage: &fluxdb_storage::FileStorage) -> fluxdb_core::Result<usize> {
    let mut purged = 0usize;
    purged += purge_orphans_in(
        storage,
        &storage.root_dir().join("ai-artifacts"),
        |conn| {
            let mut stmt = conn
                .prepare("SELECT relative_path FROM ai_artifacts WHERE relative_path IS NOT NULL")
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
        },
        "产物",
    )?;
    purged += purge_orphans_in(
        storage,
        &storage.root_dir().join("ai-results").join("results"),
        |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT relative_path FROM ai_results
                      WHERE retention_kind='retained' AND relative_path IS NOT NULL",
                )
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
            // 结果行的相对路径形如 `results/<uuid>.json`：只比较文件名。
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
        },
        "结果",
    )?;
    Ok(purged)
}

/// 扫描一个受控目录下的直接子文件，删除不在 `known` 集合内、且名形如 `<uuid>.json` 的孤儿。
fn purge_orphans_in(
    storage: &fluxdb_storage::FileStorage,
    directory: &std::path::Path,
    known_query: impl Fn(&rusqlite::Connection) -> fluxdb_core::Result<Vec<String>>,
    label: &str,
) -> fluxdb_core::Result<usize> {
    if !directory.is_dir() {
        return Ok(0);
    }
    let known: std::collections::HashSet<String> = with_ai_storage(storage, known_query)?
        .into_iter()
        .filter_map(|path| {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .collect();
    let mut purged = 0usize;
    for entry in std::fs::read_dir(directory).map_err(|error| {
        Error::new(
            ErrorKind::Internal,
            format!("读取受控目录失败: {error}"),
        )
    })? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        let stem_ok = std::path::Path::new(&name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| uuid::Uuid::parse_str(stem).is_ok());
        if !stem_ok || !name.ends_with(".json") || known.contains(&name) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {
                purged += 1;
                tracing::info!(file=%name, label, "清理未登记的受控孤儿文件");
            }
            Err(error) => tracing::warn!(file=%name, %error, label, "孤儿文件清理失败"),
        }
    }
    Ok(purged)
}

/// 受控产物相对路径校验：单段 `<uuid>.json`，与写入侧同一约束。
fn is_controlled_artifact_file(relative: &str) -> bool {
    let path = std::path::Path::new(relative);
    path.components().count() == 1
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| uuid::Uuid::parse_str(stem).is_ok())
        && path.extension() == Some(std::ffi::OsStr::new("json"))
}

/// 受控结果相对路径校验：`results/<uuid>.json`，与写入侧同一约束。
fn is_controlled_result_file(relative: &str) -> bool {
    let path = std::path::Path::new(relative);
    let parts: Vec<_> = path.components().collect();
    parts.len() == 2
        && parts[0] == std::path::Component::Normal(std::ffi::OsStr::new("results"))
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| uuid::Uuid::parse_str(stem).is_ok())
        && path.extension() == Some(std::ffi::OsStr::new("json"))
}
