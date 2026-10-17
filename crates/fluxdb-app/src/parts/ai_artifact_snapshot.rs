// 受控产物快照的临时写入、同步与原子改名。
impl BuiltinBiProducer {
    /// 落盘渲染快照：先写临时文件再改名，避免留下半截文件被当成完整报告。
    fn write_snapshot(&self, content: &str) -> Result<ArtifactFileFact, ArtifactError> {
        std::fs::create_dir_all(&self.artifact_root).map_err(|error| {
            ArtifactError::new(
                "artifact_write_failed",
                format!("创建产物目录失败: {error}"),
            )
        })?;
        if std::fs::symlink_metadata(&self.artifact_root)
            .map_err(|_| ArtifactError::new("artifact_write_failed", "产物目录不可读取"))?
            .file_type().is_symlink()
        {
            return Err(ArtifactError::new("artifact_write_failed", "产物目录不得是符号链接"));
        }
        let name = format!("{}.json", uuid::Uuid::new_v4());
        let path = self.artifact_root.join(&name);
        let temp = self
            .artifact_root
            .join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let write = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()
        })();
        write.map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            ArtifactError::new(
                "artifact_write_failed",
                format!("写产物快照失败: {error}"),
            )
        })?;
        std::fs::rename(&temp, &path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            ArtifactError::new(
                "artifact_write_failed",
                format!("提交产物快照失败: {error}"),
            )
        })?;
        Ok(ArtifactFileFact {
            relative_path: name,
            byte_count: content.len() as i64,
            content_hash: crate::source_hash(&[content]),
        })
    }

}
