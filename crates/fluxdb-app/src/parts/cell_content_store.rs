pub mod cell_content_store {
    use std::collections::HashMap;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use fluxdb_core::{CellContentMeta, CellContentStore, Error, ErrorKind, LargeValueKind, Result};

    /// 单归属（一个结果页/一个标签）的内容总配额。
    pub const CELL_CONTENT_OWNER_QUOTA_BYTES: u64 = 256 * 1024 * 1024;
    /// 进程内所有归属的合计配额。
    pub const CELL_CONTENT_TOTAL_QUOTA_BYTES: u64 = 1024 * 1024 * 1024;
    /// 上传替换暂存内容的独立归属名。
    ///
    /// 必须独立于结果页归属：结果页按 `release_owner` 整批释放，若共用归属，
    /// 用户正在准备上传的那份内容会因为同表换页而被一起删掉。
    pub const UPLOAD_STAGING_OWNER: &str = "upload-staging";

    #[derive(Debug)]
    struct Entry {
        path: PathBuf,
        owner: String,
        byte_length: u64,
        kind: LargeValueKind,
    }

    pub struct CellContentFileStore {
        root: PathBuf,
        inner: Mutex<Inner>,
    }

    #[derive(Default)]
    struct Inner {
        entries: HashMap<String, Entry>,
        owner_bytes: HashMap<String, u64>,
        total_bytes: u64,
    }

    impl CellContentFileStore {
        /// `root` 是应用存储根目录（`FileStorage::root_dir()`），实际目录为 `<root>/cell-contents`。
        pub fn new(root: impl AsRef<Path>) -> Self {
            Self {
                root: root.as_ref().join("cell-contents"),
                inner: Mutex::new(Inner::default()),
            }
        }

        fn ensure_dir(&self) -> Result<()> {
            fs::create_dir_all(&self.root).map_err(|error| {
                Error::new(ErrorKind::Internal, format!("创建大字段内容目录失败：{error}"))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // 私有目录：其他用户读不到字段正文。
                fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700)).map_err(|error| {
                    Error::new(ErrorKind::Internal, format!("设置大字段内容目录权限失败：{error}"))
                })?;
            }
            Ok(())
        }

        fn owner_dir(&self, owner: &str) -> PathBuf {
            self.root.join(format!("{:016x}", owner_hash(owner)))
        }

        fn extension(kind: LargeValueKind) -> &'static str {
            match kind {
                LargeValueKind::Text => "txt",
                LargeValueKind::Json => "json",
                LargeValueKind::Binary => "bin",
            }
        }

        /// 配额校验：配额不足时报错，让调用方把页标成资源不完整，而不是丢行。
        fn reserve(&self, owner: &str, incoming: u64) -> Result<()> {
            let inner = self
                .inner
                .lock()
                .map_err(|_| Error::new(ErrorKind::Internal, "大字段内容存储状态损坏"))?;
            let owner_used = inner.owner_bytes.get(owner).copied().unwrap_or(0);
            if owner_used.saturating_add(incoming) > CELL_CONTENT_OWNER_QUOTA_BYTES {
                // 归属既可能是某个结果页/表页，也可能是上传暂存，文案不能写死「结果」。
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!(
                        "大字段内容超过单个归属的配额（{} MiB），无法继续保留；请缩小查询范围或关闭部分结果后再试",
                        CELL_CONTENT_OWNER_QUOTA_BYTES / (1024 * 1024)
                    ),
                ));
            }
            if inner.total_bytes.saturating_add(incoming) > CELL_CONTENT_TOTAL_QUOTA_BYTES {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!(
                        "超大字段内容总量超过本进程配额（{} MiB），请关闭部分结果后再试",
                        CELL_CONTENT_TOTAL_QUOTA_BYTES / (1024 * 1024)
                    ),
                ));
            }
            Ok(())
        }
    }

    impl CellContentFileStore {
        /// 落盘一份内容并登记。`expected` 已知时先按它占配额（一次判断，失败即返回）；
        /// 未知时按块累计判断，超配额立刻删文件退出，绝不写出半份留档。
        fn store_from_reader(
            &self,
            owner: &str,
            kind: LargeValueKind,
            reader: &mut dyn Read,
            expected: Option<u64>,
        ) -> Result<String> {
            self.ensure_dir()?;
            if let Some(expected) = expected {
                self.reserve(owner, expected)?;
            }

            let id = format!("{:016x}{:016x}", owner_hash(owner), random_id());
            let dir = self.owner_dir(owner);
            fs::create_dir_all(&dir)
                .map_err(|error| Error::new(ErrorKind::Internal, format!("创建归属目录失败：{error}")))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            }
            let path = dir.join(format!("{id}.{}", Self::extension(kind)));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path).map_err(|error| {
                Error::new(ErrorKind::Internal, format!("创建大字段内容文件失败：{error}"))
            })?;

            // 拷贝缓冲固定为 1 MiB：内存占用与内容大小无关，这是流式暂存的意义。
            const COPY_BUFFER_BYTES: usize = 1024 * 1024;
            let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
            let mut written: u64 = 0;
            loop {
                let read = match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) => {
                        let _ = fs::remove_file(&path);
                        return Err(Error::new(
                            ErrorKind::Internal,
                            format!("读取待写入内容失败：{error}"),
                        ));
                    }
                };
                written = written.saturating_add(read as u64);
                // 大小未知时按块校验配额：超限当场停，不留半份文件。
                if expected.is_none()
                    && let Err(error) = self.reserve(owner, written)
                {
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                if let Err(error) = file.write_all(&buffer[..read]) {
                    let _ = fs::remove_file(&path);
                    return Err(Error::new(
                        ErrorKind::Internal,
                        format!("写入大字段内容失败（磁盘可能已满）：{error}"),
                    ));
                }
            }
            if let Err(error) = file.sync_all() {
                let _ = fs::remove_file(&path);
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("写入大字段内容失败（磁盘可能已满）：{error}"),
                ));
            }
            drop(file);

            let mut inner = self
                .inner
                .lock()
                .map_err(|_| Error::new(ErrorKind::Internal, "大字段内容存储状态损坏"))?;
            inner.total_bytes = inner.total_bytes.saturating_add(written);
            *inner.owner_bytes.entry(owner.to_string()).or_default() += written;
            inner.entries.insert(
                id.clone(),
                Entry {
                    path,
                    owner: owner.to_string(),
                    byte_length: written,
                    kind,
                },
            );
            Ok(id)
        }
    }

    impl CellContentStore for CellContentFileStore {
        fn put(&self, owner: &str, kind: LargeValueKind, bytes: &[u8]) -> Result<String> {
            self.store_from_reader(owner, kind, &mut &bytes[..], Some(bytes.len() as u64))
        }

        fn put_from_reader(
            &self,
            owner: &str,
            kind: LargeValueKind,
            reader: &mut dyn Read,
        ) -> Result<String> {
            self.store_from_reader(owner, kind, reader, None)
        }

        fn stat(&self, content_id: &str) -> Option<CellContentMeta> {
            let inner = self.inner.lock().ok()?;
            let entry = inner.entries.get(content_id)?;
            Some(CellContentMeta {
                byte_length: entry.byte_length,
                kind: entry.kind,
            })
        }

        fn read(&self, content_id: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
            let path = {
                let inner = self
                    .inner
                    .lock()
                    .map_err(|_| Error::new(ErrorKind::Internal, "大字段内容存储状态损坏"))?;
                let entry = inner.entries.get(content_id).ok_or_else(|| {
                    Error::new(
                        ErrorKind::Query,
                        "大字段内容已失效（结果已关闭或已翻页），请重新执行查询",
                    )
                })?;
                entry.path.clone()
            };
            let mut file = File::open(&path)
                .map_err(|error| Error::new(ErrorKind::Internal, format!("打开大字段内容失败：{error}")))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| Error::new(ErrorKind::Internal, format!("定位大字段内容失败：{error}")))?;
            let mut buffer = vec![0u8; len];
            let mut filled = 0usize;
            while filled < len {
                match file.read(&mut buffer[filled..]) {
                    Ok(0) => break,
                    Ok(read) => filled += read,
                    Err(error) => {
                        return Err(Error::new(
                            ErrorKind::Internal,
                            format!("读取大字段内容失败：{error}"),
                        ));
                    }
                }
            }
            buffer.truncate(filled);
            Ok(buffer)
        }

        fn release(&self, content_id: &str) {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            let Some(entry) = inner.entries.remove(content_id) else {
                return;
            };
            let _ = fs::remove_file(&entry.path);
            inner.total_bytes = inner.total_bytes.saturating_sub(entry.byte_length);
            if let Some(used) = inner.owner_bytes.get_mut(&entry.owner) {
                *used = used.saturating_sub(entry.byte_length);
                if *used == 0 {
                    inner.owner_bytes.remove(&entry.owner);
                }
            }
        }

        fn release_owner(&self, owner: &str) {
            let ids: Vec<String> = {
                let Ok(inner) = self.inner.lock() else {
                    return;
                };
                inner
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.owner == owner)
                    .map(|(id, _)| id.clone())
                    .collect()
            };
            for id in ids {
                self.release(&id);
            }
            let _ = fs::remove_dir_all(self.owner_dir(owner));
        }
    }

    /// 内容存储是进程级单例：测试里注册自己的存储时必须串行，否则并行用例会
    /// 读到别的用例注册的实现，表现为「内容不存在」的假失败。
    #[cfg(test)]
    pub fn lock_content_store_for_test() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 清理上次进程崩溃遗留的内容目录。
    ///
    /// 结果页内容只对产生它的那次会话有效，跨进程没有意义；不清理会长期占盘。
    pub fn sweep_stale_cell_contents(root: impl AsRef<Path>) -> Result<()> {
        let dir = root.as_ref().join("cell-contents");
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::new(
                ErrorKind::Internal,
                format!("清理大字段内容目录失败：{error}"),
            )),
        }
    }

    /// 归属目录名：稳定哈希，避免把标签名/表名写进路径。
    fn owner_hash(owner: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        owner.hash(&mut hasher);
        hasher.finish()
    }

    /// 不透明 id 的随机部分：时间戳 + 进程内计数器，避免为此引入随机数依赖。
    fn random_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0);
        nanos.rotate_left(16) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    #[cfg(test)]
    mod cell_content_store_tests {
        use super::*;

        /// 临时根目录：用完删掉。用标准库拼目录，避免为一个测试新增依赖。
        struct TempRoot(PathBuf);

        impl TempRoot {
            fn new(tag: &str) -> Self {
                let path = std::env::temp_dir().join(format!(
                    "fluxdb-cell-content-{tag}-{}-{}",
                    std::process::id(),
                    random_id()
                ));
                fs::create_dir_all(&path).expect("create temp root");
                Self(path)
            }
        }

        impl Drop for TempRoot {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        fn store(tag: &str) -> (TempRoot, CellContentFileStore) {
            let dir = TempRoot::new(tag);
            let store = CellContentFileStore::new(&dir.0);
            (dir, store)
        }

        #[test]
        fn put_stat_read_round_trip() {
            let (_dir, store) = store("round-trip");
            let payload = b"abcdefghij";
            let id = store.put("tab-1", LargeValueKind::Text, payload).expect("put");
            // 句柄里不能出现路径分隔符或文件名痕迹。
            assert!(!id.contains('/') && !id.contains('\\'), "{id}");
            let meta = store.stat(&id).expect("stat");
            assert_eq!(meta.byte_length, payload.len() as u64);
            assert_eq!(store.read(&id, 3, 4).expect("read"), b"defg");
            // 越界读取只返回剩余字节。
            assert_eq!(store.read(&id, 8, 100).expect("read"), b"ij");
        }

        #[test]
        fn release_invalidates_handle() {
            let (_dir, store) = store("release");
            let id = store.put("tab-1", LargeValueKind::Json, b"{}").expect("put");
            store.release(&id);
            assert!(store.stat(&id).is_none());
            assert!(store.read(&id, 0, 1).is_err());
            // 重复释放安全。
            store.release(&id);
        }

        #[test]
        fn release_owner_drops_every_handle() {
            let (_dir, store) = store("owner");
            let first = store.put("tab-1", LargeValueKind::Text, b"a").expect("put");
            let second = store.put("tab-1", LargeValueKind::Text, b"b").expect("put");
            let other = store.put("tab-2", LargeValueKind::Text, b"c").expect("put");
            store.release_owner("tab-1");
            assert!(store.stat(&first).is_none());
            assert!(store.stat(&second).is_none());
            assert!(store.stat(&other).is_some());
        }

        #[test]
        fn owner_quota_is_enforced_without_partial_write() {
            let (_dir, store) = store("quota");
            let big = vec![0u8; CELL_CONTENT_OWNER_QUOTA_BYTES as usize + 1];
            let error = store
                .put("tab-1", LargeValueKind::Text, &big)
                .expect_err("quota");
            assert_eq!(error.kind, ErrorKind::Unsupported);
            // 失败不留半份文件：配额没被占用。
            let ok = store.put("tab-1", LargeValueKind::Text, b"small").expect("put");
            assert!(store.stat(&ok).is_some());
        }

        #[test]
        fn stored_file_name_has_no_business_text() {
            let (dir, store) = store("naming");
            let id = store
                .put("secret_table_name", LargeValueKind::Text, b"hello")
                .expect("put");
            let files = walk(&dir.0);
            assert!(!files.is_empty());
            assert!(
                files
                    .iter()
                    .all(|path| !path.to_string_lossy().contains("secret_table_name"))
            );
            assert!(store.stat(&id).is_some());
        }

        fn walk(dir: &Path) -> Vec<PathBuf> {
            let mut out = Vec::new();
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        out.extend(walk(&path));
                    } else {
                        out.push(path);
                    }
                }
            }
            out
        }
    }
}

pub use cell_content_store::{CellContentFileStore, sweep_stale_cell_contents};
