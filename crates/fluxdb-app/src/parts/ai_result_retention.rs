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
        let path = std::path::Path::new(&relative);
        let parts: Vec<_> = path.components().collect();
        let safe_name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| uuid::Uuid::parse_str(stem).is_ok());
        if parts.len() != 2
            || parts[0] != std::path::Component::Normal(std::ffi::OsStr::new("results"))
            || !safe_name
            || path.extension() != Some(std::ffi::OsStr::new("json"))
        {
            tracing::warn!(result_id=%id, "拒绝清理非法结果相对路径");
            continue;
        }
        let directory = root.join("results");
        if directory.exists() {
            let controlled =
                std::fs::canonicalize(&root).map_err(result_io_error("结果根目录不可用"))?;
            let actual =
                std::fs::canonicalize(&directory).map_err(result_io_error("结果目录不可用"))?;
            if !actual.starts_with(&controlled) {
                return Err(Error::new(ErrorKind::Internal, "结果目录已越过受控根"));
            }
        }
        match std::fs::remove_file(root.join(path)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(result_id=%id, %error, "过期结果文件清理失败，留待重试");
                continue;
            }
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
