mod cell_content {
    use super::*;

    use std::io::Write;

    use fluxdb_core::{CellContentStore, Error, ErrorKind, LargeValueKind};

    impl AppController {
        /// 读取大字段内容引用的一块；`offset` 由面板给出（窗口末尾），返回对齐字符边界的块。
        ///
        /// 只接受不透明 `content_id`：不认路径、不按行身份重查数据库，块的来源始终是
        /// 取数时落下的那一份不可变内容。引用已随结果关闭释放时返回错误。
        pub(crate) fn load_cell_content_chunk(
            &self,
            content_id: &str,
            kind: LargeValueKind,
            offset: u64,
        ) -> fluxdb_core::Result<CellContentChunk> {
            let store = cell_content_store_or_error()?;
            let bytes = store.read(content_id, offset, fluxdb_core::CELL_CHUNK_BYTES)?;
            if bytes.is_empty() {
                return Ok(CellContentChunk {
                    offset,
                    text: String::new(),
                    byte_length: 0,
                });
            }
            let (text, consumed) = decode_content_chunk(kind, &bytes);
            Ok(CellContentChunk {
                offset,
                text,
                byte_length: consumed,
            })
        }

        /// 把完整内容流式写到用户选定文件（`.partial` 收尾后改名，避免留半成品）。
        ///
        /// 逐块读、逐块写：单块上限 [`fluxdb_core::CELL_CHUNK_BYTES`]，内存占用与内容大小无关。
        pub(crate) fn save_cell_content_to_file(
            &self,
            content_id: &str,
            path: &std::path::Path,
        ) -> fluxdb_core::Result<u64> {
            let store = cell_content_store_or_error()?;
            let mut partial = path.as_os_str().to_os_string();
            partial.push(".partial");
            let partial = std::path::PathBuf::from(partial);
            let mut file = std::fs::File::create(&partial)
                .map_err(|error| Error::new(ErrorKind::Internal, format!("创建文件失败：{error}")))?;
            let mut offset = 0u64;
            let mut written = 0u64;
            loop {
                let bytes = store.read(content_id, offset, fluxdb_core::CELL_CHUNK_BYTES)?;
                if bytes.is_empty() {
                    break;
                }
                if let Err(error) = file.write_all(&bytes) {
                    let _ = std::fs::remove_file(&partial);
                    return Err(Error::new(
                        ErrorKind::Internal,
                        format!("写入文件失败（磁盘可能已满）：{error}"),
                    ));
                }
                offset += bytes.len() as u64;
                written += bytes.len() as u64;
            }
            file.sync_all()
                .map_err(|error| Error::new(ErrorKind::Internal, format!("写入文件失败：{error}")))?;
            drop(file);
            std::fs::rename(&partial, path).map_err(|error| {
                let _ = std::fs::remove_file(&partial);
                Error::new(ErrorKind::Internal, format!("保存文件失败：{error}"))
            })?;
            Ok(written)
        }
    }
    /// 内容存储未注册（未启动完成/测试环境）时给出明确错误，而不是空块冒充读完。
    ///
    /// 文案固定不带动作名：同一条消息要覆盖读取、保存与上传暂存，动作名只能靠
    /// 上下文区分，拼进消息反而会让展示层无法按字面匹配做本地化。
    fn cell_content_store_or_error() -> fluxdb_core::Result<std::sync::Arc<dyn CellContentStore>> {
        fluxdb_core::cell_content_store().ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                "大字段内容存储未初始化，无法读写完整内容",
            )
        })
    }
}

#[cfg(test)]
mod cell_content_chunk_tests {
    use super::*;
    use crate::cell_content_store::CellContentFileStore;
    use fluxdb_core::{CellContentStore as _, LargeValueKind};

    /// 临时根目录：只清理本用例创建的内容。
    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fluxdb-cell-content-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp root");
            Self(dir)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 分块必须拼回原文、块边界落在 UTF-8 字符上，保存结果与原文逐字节一致，
    /// 引用释放后再读必须失败（不能拿旧引用读别的结果）。
    #[test]
    fn chunks_reassemble_original_and_reference_dies_with_owner() {
        let root = TempRoot::new("chunks");
        let store = std::sync::Arc::new(CellContentFileStore::new(&root.0));
        fluxdb_core::register_cell_content_store(store.clone());
        let controller = AppController::new();
        // 每块 256 KiB，多字节字符跨块边界：内容长度刻意不是块大小的整数倍。
        let payload: String = "汉字abc".repeat(120_000);
        let content_id = store
            .put("result-test-1", LargeValueKind::Text, payload.as_bytes())
            .expect("put");

        let mut offset = 0u64;
        let mut rebuilt = String::new();
        loop {
            let chunk = controller
                .load_cell_content_chunk(&content_id, LargeValueKind::Text, offset)
                .expect("chunk");
            if chunk.byte_length == 0 {
                break;
            }
            // 每块都必须能被完整解码：块边界不能切在多字节字符中间。
            assert_eq!(
                chunk.text.len() as u64,
                chunk.byte_length,
                "块必须落在字符边界上"
            );
            offset = chunk.offset + chunk.byte_length;
            rebuilt.push_str(&chunk.text);
            assert!(offset <= payload.len() as u64);
        }
        assert_eq!(rebuilt, payload, "分块拼接必须等于原内容");
        assert!(
            offset > fluxdb_core::CELL_CHUNK_BYTES as u64,
            "内容必须真的分块"
        );

        let target = root.0.join("saved.txt");
        let written = controller
            .save_cell_content_to_file(&content_id, &target)
            .expect("save");
        assert_eq!(written, payload.len() as u64);
        assert_eq!(
            std::fs::read(&target).expect("saved file"),
            payload.as_bytes(),
            "流式保存必须逐字节等于原内容"
        );
        assert!(
            !root.0.join("saved.txt.partial").exists(),
            "保存成功后不能留下半成品"
        );

        store.release_owner("result-test-1");
        assert!(
            controller
                .load_cell_content_chunk(&content_id, LargeValueKind::Text, 0)
                .is_err(),
            "结果关闭后旧引用必须失效"
        );
    }
}
