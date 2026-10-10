# T026 Windows 附件安全读取（2026-10-10）

平台：开发机 macOS arm64。**未在真实 Windows 机器上编译、运行或做窗口验收**；本记录只覆盖实现与可在本机完成的验证。

## 改动

- `crates/fluxdb-app/src/parts/ai_references.rs::open_ai_authorized_file`：原 `#[cfg(not(unix))]` 显式返回「附件读取不可用」，改为 `#[cfg(windows)]` 实现。
- `crates/fluxdb-app/Cargo.toml`：新增 `[target.'cfg(target_os = "windows")'.dependencies] windows = { version = "0.57", features = ["Win32_Storage_FileSystem", "Win32_Security"] }`，与 `apps/fluxdb-desktop` 已锁定的 windows 0.57 同版本，`Cargo.lock` 无新增版本。
- `crates/fluxdb-app/tests/ai_session_model_selection.rs`：`windows_file_read_*` 从「断言不可用」改为「读回真实内容」+「末端/中间 reparse point 替换均拒绝」。

## 安全模型

Unix 侧用逐级 `openat(O_NOFOLLOW|O_DIRECTORY)`，父 fd 传给下一级。Win32 没有 `openat`，`CreateFileW` 只接受路径字符串、无法接收父句柄，因此采用两层防护：

1. 逐级下钻，每级 `CreateFileW` 带 `FILE_FLAG_OPEN_REPARSE_POINT`，再用 `GetFileInformationByHandle` 显式检查 `FILE_ATTRIBUTE_REPARSE_POINT`（junction/symlink）并拒绝。该标志只禁止跟随、不禁止打开，所以属性检查是必需的，不能只依赖标志。
2. 末端用 `GetFinalPathNameByHandleW(VOLUME_NAME_DOS)` 取真实落点，与授权路径逐码元比对（ASCII 区间折叠大小写）。授权路径来自 `Path::canonicalize`，Windows 上 std 同样返回 `\\?\C:\…` 形式（`std/src/sys/fs/windows.rs::get_path` 用同一个 `VOLUME_NAME_DOS` 调用），两侧同形，可直接比对。

访问权：中间目录用 `dwDesiredAccess = 0` + `FILE_FLAG_BACKUP_SEMANTICS`（目录 ACL 通常不含 `GENERIC_READ`，请求会被拒）；只有末端请求 `GENERIC_READ`，供上层 `read_to_end` 使用。

`HANDLE` 是 `Copy` 且无 `Drop`，实现内所有放弃句柄的路径都经 `into_file` 转成 `std::fs::File` 以确保关闭。

## 本机实际执行的验证

- `cargo fmt --all -- --check` 通过。
- Windows 类型检查：因 `~/.rustup` 为 root 所有且沙箱内无网络，复制 toolchain 到可写 `RUSTUP_HOME`、手动安装 `rust-std-x86_64-pc-windows-msvc` 后，用独立 crate（仅依赖 `windows 0.57`）编译本函数的 `#[cfg(windows)]` 分支，`cargo check --target x86_64-pc-windows-msvc` 通过（仅 `dead_code` 警告）。该过程修正了 8 处真实类型/API 错误：`HANDLE.0` 是 `isize` 不能直接传给 `from_raw_handle`、`u16` 无 `to_ascii_lowercase`/`eq_ignore_ascii_case`、`&[u16]` 无 `eq_ignore_ascii_case`、`CreateFileW` 受 `Win32_Security` feature 门控、`u8` 与 `Vec<u16>` 类型不匹配、双重解引用等。
- Unix 侧回归（本机）：`cargo test -p fluxdb-app --offline --test ai_session_model_selection file_` → `file_references` 11/11 通过，确认未影响既有 Unix 行为。

## 未验证（必须在 Windows 上执行）

- `cargo check -p fluxdb-app --target x86_64-pc-windows-msvc` / `cargo test`（整 crate 依赖 `ring` 的 C 工具链，本机无法完成）。
- 新增两个 Windows 用例的实际运行：真实文件读取、末端 symlink 替换拒绝、中间目录 junction/symlink 替换拒绝。
- 真实窗口 E2E（附件选择器 → `file.read`/`file.search`）与明暗/窄窗主题。
- 目录 ACL 受限场景、中文路径、网络驱动器（UNC `\\server\share`）下的路径比较。
- Linux：走 `#[cfg(unix)]` 既有实现，本轮未改代码，仍需一次 Linux 平台验收。

---

# 追加：CSV 分段与二进制反馈（同日）

## 改动

- 新增 `crates/fluxdb-app/src/parts/ai_file_content.rs`（由 `src/lib.rs` 以 `include!` 接入，沿用现有 `parts/` 组织方式）。
- `crates/fluxdb-app/Cargo.toml` 新增 `csv = "1.4"`（`Cargo.lock` 仅 +23 行，新增 `csv`/`csv-core` 两个包，`itoa`/`ryu`/`memchr`/`serde_core` 均为既有锁定版本，无无关升降级）。
- `authorize_ai_file`：取文件头 8 KiB 判定内容类型（UTF-8 文本 / CSV·TSV / 二进制），`binary` 标记写入宿主 `source`，摘要按类型给出「按需分段读取」或「仅返回类型与预览」。运行快照仍只有 opaque id/标签/摘要，路径与类型判定不外泄。
- `file.read`：CSV/TSV 走记录分段并新增 `max_records`（schema 1–256）；文本按字节分段但在 UTF-8 字符边界安全截断；二进制只返回 `content_type`/`size`/`preview`（前 64 字节可打印字符 + 可打印比例），不回传原始字节。

## 修复的真实缺陷

1. 二进制判定原先基于「当前读取段」，`offset` 落在多字节字符中间时该段必然不是合法 UTF-8，会把合法文本附件误判成二进制。改为以授权时头部取样的 `grant.binary` 为准（缺失时回退到重读头部，不用当前段）。
2. CSV `truncated` 原按 `consumed < total_len` 判断，末条记录后解析器位置可能仍小于总长而误报「已读完」，导致模型漏掉尾部记录。改为预读一条判断是否真有剩余记录，`next_offset` 在无剩余时取 `total_len`。
3. CSV `next_offset` 曾因多读一条记录把 `reader.position()` 推过头而跳过一条数据记录。已回退为 `position()` 原生语义（下一条未读记录起点）。

## 本机实际执行的验证（macOS arm64）

`cargo fmt --all -- --check`、`cargo check --workspace --locked --offline` 通过；`cargo test -p fluxdb-app --locked --offline` 全绿（lib 740 passed / 0 failed / 1 ignored，集成各组全通过）。`file_references` 由 11 增至 16 个用例，新增：CSV 记录分段与分页续读（含引号转义+嵌入换行）、CSV 未闭合引号按数据处理不退回文本硬解、`max_records` 上限拒绝、文本多字节边界截断、二进制内容类型反馈与授权摘要不含路径。
