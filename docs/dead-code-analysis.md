# 死代码 / 可删除代码分析报告

> 分析对象：fluxDB Cargo workspace（`apps/fluxdb-desktop` 与 `crates/fluxdb-app`、
> `fluxdb-connectors`、`fluxdb-core`、`fluxdb-editor-core`、`fluxdb-editor-language`、
> `fluxdb-storage`）。
> 本次仅做分析，**未删除任何文件**。
> 验证平台：macOS（Darwin arm64），rustc/cargo 1.97.0，`--locked`。
> 行动需人工复核后执行；删除后需重新 `cargo check` / 跑测试确认。

## 方法

1. **编译期告警**（高置信度）：`find <crate>/src -name '*.rs' -exec touch {} +` 强制重编，
   再 `cargo check -p <crate> --locked`，收集 `dead_code / unused / never used /
   never constructed / field is never read / variant is never constructed`。
2. **显式标注**：全仓库 `rg "allow(dead_code)"`，逐条核对被标注条目是否真的无引用
   （大量标注已过时，标注项其实在用，已在文档中单列）。
3. **公共 API 交叉引用**（中置信度）：对 `pub fn / struct / enum / trait / const / use`
   条目用 `rg` 全 workspace（排除 `target` / `docs` / `*.md`）统计引用，0 引用者列为
   「疑似未使用」。因各 crate 多为 lib crate，`pub` 项不会触发编译器告警，这一层是
   主要的判断来源。
4. **死文件检查**：核对每个 `.rs` 是否被 `mod` / `include!` 引用。

## 总体结论

- 各 crate 的 `cargo check`（普通、`--tests`、`--all-targets`）**均零告警**，编译期
  无未抑制的死代码。项目里的"死代码"绝大多数是**显式 `#[allow(dead_code)]`** 标注项
  （全仓库共 79 处）+ 未被引用的 `pub` 公开 API。
- 显式标注的 79 处中，**相当一部分是过时标注**（被标注条目其实已在生产/测试中使用），
  真正生产死代码的高置信度项集中在少数几处（见各 crate 高置信度分组）。
- 发现 1 个整体无消费方的孤儿 crate（`fluxdb-editor-language`）。
- 未发现孤立（未进入 `mod` / `include!` 树）的 `.rs` 死文件。

---

## 1. 高置信度（可删除，仓库内无任何引用或仅测试自引用）

### 可整体删除的孤儿 crate

- **`crates/fluxdb-editor-language`（整个 crate）**
  - 唯一导出项 `LanguageAdapter`（`src/lib.rs:9`）只在自身定义与自身测试中出现，
    无任何生产调用方；`LanguageAdapter` / `fluxdb_editor_language` 符号在整个
    workspace 无引用。
  - 唯一"引用"是 `apps/fluxdb-desktop/Cargo.toml:19` 声明了
    `fluxdb-editor-language = { path = ... }`，但 **desktop 代码从未使用它**，属
    声明而未用的依赖。
  - 结论：可删除 `crates/fluxdb-editor-language/`，并同步从
    `apps/fluxdb-desktop/Cargo.toml` 与 workspace `Cargo.toml` members 移除该成员。

### 高置信度具体项

| 条目 | 位置 | 说明 |
|---|---|---|
| `_keep_query_request_visible` | `crates/fluxdb-app/src/parts/table_info.rs:215` | 空 stub，`#[allow(dead_code)]`，全仓库 0 引用 |
| `_assert_pg_role_draft_ui_ext_usable` | `crates/fluxdb-app/src/parts/state.rs:634` | 空 stub，`#[allow(dead_code)]`，全仓库 0 引用 |
| `AiConversationProvider::for_test` | `crates/fluxdb-app/src/parts/ai_conversation_provider.rs:84` | `pub fn for_test`，除定义外 0 引用 |
| `pub const DEFAULT_COMPRESSION_TRIGGER_PERCENT` | `crates/fluxdb-storage/src/parts/ai_platform/agents.rs:13` | 全 workspace 0 引用 |
| `pub fn bump_iteration` | `crates/fluxdb-storage/src/parts/ai_platform/sessions.rs:807` | 全 workspace 0 引用，未接线（iteration 实际由其他路径递增） |
| `pub fn expire_approval` | `crates/fluxdb-storage/src/parts/ai_platform/audits.rs:418` | 全 workspace 0 引用（过期实际由 `find_valid_approval` 等路径处理） |
| `pub fn statement_action_decorations` | `apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/decorations.rs:12` | 仅定义 + 自身测试引用，生产死代码 |
| `fn masked_ranges` | `apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/completion.rs:298` | 仅定义 + 自身测试引用，生产死代码 |
| `pub fn row_for_offset` | `apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/statements.rs:156` | `#[allow(dead_code)]`，0 引用，纯 stub |
| `pub fn dirty_ranges_for_edit`（及其私有 `floor_char_boundary`） | `apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/syntax.rs:2305/2335` | 仅定义 + 测试引用，生产死代码 |
| `fn highlight_sql_tree_sitter_cached` | `apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/syntax.rs:1120` | 仅测试引用，生产走非缓存路径 |

> 注：上表「生产死代码/仅测试」各项，若确需保留给单测或用例，建议移入 `#[cfg(test)]`
> 测试模块，而不是保留带 `#[allow(dead_code)]` 的生产定义。

---

## 2. 中置信度（疑似未使用公开 API / over-export，可降可见性或按业务确认后删）

这些条目为 `pub`，但整个 workspace（含仓库内部与 desktop 调用方）除定义处/测试外
**无任何引用**。删除会破坏内部调用，故更可能的情况是"暴露过度"，可改为
`pub(crate)` 或私有；如确认未来也无消费者，则可删除。

### fluxdb-core

- `BackupFormat::is_sql` — `src/parts/backup_restore.rs:20`
- `BackupExecution::as_record` — `src/parts/backup_restore.rs:39`
- `ConnectionConfig::has_redis_profile` — `src/parts/connection.rs:40`
- `ConnectionConfig::has_mysql_profile` — `src/parts/connection.rs:67`
- `ConnectionConfig::has_postgres_profile` — `src/parts/connection.rs:94`
- `const MYSQL_LEGACY_OPTION_KEYS` — `src/parts/mysql_profile.rs:193`
- `const POSTGRES_LEGACY_OPTION_KEYS` — `src/parts/postgres_profile.rs:256`
- `const REDIS_LEGACY_OPTION_KEYS` — `src/parts/redis_profile.rs:174`
- `is_secret_option_key` — `src/parts/redis_profile.rs:161`（doc 声称 storage
  的 `strip_plaintext_secrets` 复用，但 `crates/fluxdb-storage/src/lib.rs:996` 的私有
  实现并未调用它，注释与实现不符）
- `DatabaseUserAdminProvider::alter_login_sql` — `src/parts/user_admin.rs:261`
- `DatabaseUserAdminProvider::default_privileges_for_scope` — `src/parts/user_admin.rs:362`

### fluxdb-editor-core

- `BufferSnapshot::clamp_offset` — `src/buffer.rs:842`
- `DisplayMap::clear_blocks` — `src/display_map.rs:1041`
- `DisplayMap::block_snapshot` — `src/display_map.rs:1048`
- `LayerPatch::old_bounds` — `src/layer.rs:256`
- `LayerPatch::new_bounds` — `src/layer.rs:262`
- `Range::contains_inclusive` — `src/model.rs:68`

### fluxdb-connectors

- `pub fn format_for_execution` — `src/parts/backup_restore/format.rs:12`（生产仅由
  `backup_restore.rs:24` 重新导出，无外部消费）
- `pub fn execution_from_record` — `src/parts/backup_restore/format.rs:72`（同上）
- `pub fn detect_backup_format` — `src/parts/backup_restore/format.rs:55`（内容部被
  `restore.rs:23` 使用，但 `pub` 暴露无必要）
- `pub fn is_sqlite_binary_bytes` — `src/parts/backup_restore/format.rs:47`（仅同文件内部
  调用，`pub` 暴露无必要，可降为私有）

### fluxdb-app（over-export 主要项，均可降可见性，非直接删除）

- AI 工具类：`AiSqlExecuteTool`/`AiSqlReadTool`/`AiSqlSearchTool`/`AiSqlDescribeTool`
  （`parts/ai_sql_exec.rs`，均仅在同文件 `catalog.register(...)` 内部注册）；
  `AiRedisExecuteTool`/`AiRedisInspectTool`/`AiRedisScanTool`/`AiRedisServerInfoTool`/
  `AiRedisTarget`（`parts/ai_redis_exec.rs`）
- AI 设置：`ProviderId`/`resolve_provider`/`stored_id`（`parts/controller/ai_settings.rs`）；
  `fetch_ai_models`（`parts/controller/ai_model_network.rs:169`，仅测试引用）
- AI 执行/权限：`execute_sql_with_parameters`（`ai_sql_exec.rs:542`）；
  `ResultError`/`ToolError`/`ValidatedArguments`/`validate_schema`（`ai_tool_registry.rs`）；
  `canonical_json_str`（`ai_permission.rs:404`）
- AI 会话：`build_title_prompt`/`normalize_session_title`（`ai_session_title.rs`）；
  `stream_delta`（`ai_conversation_stream.rs:45`）；`stream_sse_reader_emit`
  （`ai_transport.rs:178`）
- 常量（仅同文件使用）：`SQL_MAX_ROWS_DEFAULT`/`SQL_MAX_ROWS_HARD`（`ai_sql_exec.rs:70-71`）、
  `DB_*_INPUT_SCHEMA`（`ai_sql_policy.rs:468/483`）、`REDIS_*_INPUT_SCHEMA`
  （`ai_redis_policy.rs:98-141`）

---

## 3. 测试专用（生产构建为死代码，建议移入 `#[cfg(test)]`）

| 条目 | 位置 | 说明 |
|---|---|---|
| `function_completion_items` | `crates/fluxdb-app/src/parts/query_completion.rs:2544` | 生产用 `function_completion_items_for(...)`；本函数仅 `parts/tests/query.rs` 引用 |
| `column_completion_items` | `crates/fluxdb-app/src/parts/query_completion.rs:2892` | 生产用 `cte_column_completion_items` / `derived_column_completion_items`；本函数仅测试引用 |

---

## 4. 跨 crate 重复实现（去重候选，需确认职责归属）

- `redis_value_illegal_control_char` 重复实现：
  - `crates/fluxdb-connectors/src/parts/redis/util.rs:115`（`pub`，全 workspace 无外部调用）
  - `apps/fluxdb-desktop/src/main_parts/redis_detail/common.rs:482`（desktop 私有副本，
    逻辑逐字符一致；实际由 `controller_common.rs:336/345` 使用）
  - 建议删除 connectors 的未使用 `pub` 版，或让 desktop 引用 connectors 版本去重。

---

## 5. `#[allow(dead_code)]` 过时标注（标注项其实在用，仅清理提示，不应删除）

以下标注为 `#[allow(dead_code)]`，但经核实被标注条目在**生产代码**中被使用，属过时标注，
删除标注本身即可（不要删代码）：

- `crates/fluxdb-app/src/parts/query_completion.rs:8`（`NextAction` 枚举被
  `parts/controller/query_completion.rs:931/929-960` 生产使用）
- `crates/fluxdb-app/src/parts/query_completion.rs:2989`（`column_completion_documentation`
  被 `parts/completion_index.rs:884` 调用）
- `apps/fluxdb-desktop/src/main_parts/app_state.rs:2606`（`AppMessageKind` 被
  `er/toolbar.rs`、`data_table_ui/message_status.rs` 等多处使用）
- `apps/fluxdb-desktop/src/main_parts/json_editor/types.rs:115`（`end_line` 被
  `json_editor/parser.rs` 使用）
- `apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/statements.rs:265`
  （`parameter_replacement_target` 被 `navicat_main/query_parameters.rs` 使用）
- `apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/syntax.rs` 中 `SqlLanguage`/
  `SqlLanguage::new` 被 `sql_preview.rs:47` 使用；`dirty_ranges_for_edit` 以外的若干语法项
  亦在生产引用，逐条核对注释后再处理。

类似地，desktop 大量 `editor_component` 里标注"尚未接入/保留备用"的字段（如
`query_find_inputs`、`Providers`、`LineHitRegion` 等）其实已被接线使用，均属过时标注。

---

## 6. 刻意保留、不建议删除（记录备查）

- `crates/fluxdb-connectors/src/parts/redis/wire.rs:55` — `RedisConnection.ssh_tunnel`
  `#[allow(dead_code)]`：刻意作为 RAII 生命周期锚点持有 SSH 隧道句柄，连接 Drop 时关闭。
- `crates/fluxdb-storage/src/credential.rs` 中 `#[allow(dead_code)]` 的
  `CredentialError::{NotFound, Locked, Denied}` 变体：为旧「系统凭据库」语义残留，当前
  SQLite 加密后端从不构造它们；如需精简可删除这三个变体并同步收紧
  `to_storage_error`（credential.rs:100-101）的 match，属中置信度清理候选。

---

## 7. 已排除（确认不是死代码）

- **fluxdb-storage `ai_platform/schema.rs`**：设计文档 §42 唯一 schema 来源，被
  `ensure_schema` / `verify_ai_schema` 消费，非冗余。
- **`tool_calls` 模块**：虽无直接外部引用，但被 `sessions.rs:1317 pub use` 再导出，
  `load_tool_call` / `ToolCallView` / `list_tool_calls_for_turn` 等在 fluxdb-app 广泛使用。
- **`u64_hex::serialize/deserialize`**（`fluxdb-core` `object_query.rs`）：经
  `#[serde(with = "u64_hex")]` 按模块约定调用，属误报。
- **所有 crate 均无孤立 `.rs` 文件**（全部被 `mod` / `include!` 引用）。

## 7.1 `apps/fluxdb-desktop/examples/` 文件夹

`examples/` 下的 `.rs` 会被 Cargo 自动编译为独立示例二进制，但代码 / CI / scripts /
仓库内其它源码均无任何引用（`cargo check -p fluxdb-desktop --examples --locked` 通过，
两者当前可编译）。两个文件性质不同，删除策略要分开看：

| 文件 | 引用情况 | 结论 |
|---|---|---|
| `t016_acceptance_fixture.rs` | 被 `docs/testing/ai-agent/t016-real-window-2026-10-08.md:9` 明确写成 T016 真实窗口验收的 fixture 生成器 | 仅当确认不再需要复现 T016 验收时删除；否则需先改掉该文档引用 |
| `t020_acceptance_fixture.rs` | 全仓库（含所有 docs / CI / scripts / md）零引用；t020 文档实际用 `ai_panel_fixture` 而非它 | 可直接安全删除 |

**整体删除**需同步处理 `t016` 文档第 9 行的引用；仅清理不受影响的，则先删
`t020_acceptance_fixture.rs` 即可。

> 补充：git 历史显示两者分别对应 2026-10-08 的 T016 / T020 真实窗口验收提交
> （`6a26e12`、`b03da8e`），属手工运行的隔离 fixture 生成器，非自动构建产物。

---

## 8. 汇总表

| 类别 | 说明 | 数量 |
|---|---|---|
| 高置信度（可删/孤儿 crate） | 上表第 1 节全部 | ~13 处 + 1 个 crate |
| 中置信度（疑似未使用 pub API / over-export） | 第 2 节 | 4（core）+ 6（editor-core）+ 4（connectors）+ ~20+（app） |
| 测试专用（生产死代码） | 第 3 节 | 2 |
| 跨 crate 重复实现 | 第 4 节 | 1 组 |
| 过时 `#[allow(dead_code)]` 标注 | 第 5 节 | 若干（应删标注而非代码） |

## 建议的下一步

1. 先处理**高置信度**项（第 1 节）与孤儿 crate：`fluxdb-editor-language` 整体移除 +
   desktop 依赖清理。
2. 测试专用项移入 `#[cfg(test)]` 并去掉 `#[allow(dead_code)]`。
3. 过时标注逐条去掉（第 5 节），让编译器能重新监督这块代码。
4. 中置信度的 over-export（第 2 节）按需降可见性。
5. 每步都运行 `cargo check --workspace --locked` 与相关 crate 测试确认无误；macOS 删除
   `fluxdb-editor-language` 后需在 Windows/Linux 各做一次 `cargo check` 复核三端。
