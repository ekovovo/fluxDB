# T024 独立风险分类与审批回归（2026-10-09，macOS arm64）

## 范围与隔离

- Worktree：`/Users/shining3d/.codex/worktrees/t024-independent-tests/fluxDB`。
- 分支：`codex/t024-independent-tests`；基线：`d271cdbc617ac6bfc75a5a7df4dd05e7b171ea6f`。
- 本轮不修改 `agent` 工作区，不导入 T023 的未提交改动，不修改 SQL 执行服务、Connector 或 UI。
- 编译缓存在本 worktree 的 `target/`；复现与最终日志保存在 `target/t024-validation/`。
- 只使用隔离 SQLite 内存库/临时文件与既有受控 Provider/工具；不读取生产配置，不连接真实模型或 MySQL/PostgreSQL，不调用有副作用的远端函数。
- **仅完成 T024 的独立子项，不代表 T024 完整通过，不勾选任务。**

## 修复前复现

| 场景 | 预期 | 修复前实际 | 证据 |
| --- | --- | --- | --- |
| PG 双引号函数名：`pg_catalog."pg_notify"`、`"nextval"`、`pg_catalog."lo_unlink"` | 需授权，不自动读取 | 三者均被分类为 `ReadOnly` | `target/t024-validation/t024-worktree-red.log` |
| PG shared/try/xact/unlock advisory lock 变体与 MySQL `RELEASE_ALL_LOCKS()` | 需授权，不自动读取 | 漏列的函数被分类为 `ReadOnly` | 同上 |
| SQLite `PRAGMA user_version(42)`、`application_id(42)`、`schema_version(42)` | 需授权，不自动读取 | 三者均被分类为 `ReadOnly`；内存 SQLite 确认括号形式真实赋值 | 同上 |
| 实际 `AiSqlReadTool::query` 执行 `PRAGMA user_version(42)`，连接可写、初值 7 | `not_read_only`，数据库保持 7 | 回执 `Succeeded`，数据库变成 42 | `target/t024-validation/t024-worktree-red-sqlite.log` |

初始五项探针结果为 2 passed / 3 failed；随后单独的真实工具入口回归为 0 passed / 1 failed。
工具入口测试直接轮询当前生产契约的就绪 future，避免在 Connector 自带 runtime 外嵌套 Tokio runtime。

## 改动

- `crates/fluxdb-app/src/parts/ai_sql_policy.rs`：
  - 复用已有 sqlparser visitor，按 AST 函数标识符实际值检查副作用，覆盖 schema 限定、双引号、嵌套 CTE/子查询；保留原来的保守文本检查与原因码。
  - 补齐 shared/try/xact/unlock advisory lock 与 `release_all_locks` 风险分类。
  - PRAGMA 同时检查 `is_eq` 与 `value`；只有 table/index/foreign-key 等对象查询允许对象参数，设置项有值即不能走只读入口。
  - 明确注释：已知副作用集合不是纯函数白名单，不能替代数据库权限/只读事务。
- 新增 `crates/fluxdb-app/tests/ai_sql_policy_boundaries.rs`，7 项：
  - 引号函数名及嵌套表达式、副作用锁函数、PRAGMA 赋值均不能自动读取。
  - MySQL 可执行注释含导出语句不能自动读取，仅做分类、不远端执行。
  - 工具/版本/连接/schema/SQL/参数/插件摘要变化会改变审批 hash。
  - 纯函数与合法 PRAGMA 对象参数/无值设置读取仍可用。
  - 可写 SQLite 连接通过真实 `db.query` 工具拒绝三种设置赋值及限定/引号/等号变体；每次回执为 `Failed/not_read_only`，数据库初值 7 保持不变。
- `crates/fluxdb-app/tests/ai_conversation_approval_guard.rs`：
  - 新增执行载荷漂移回归：批准后不改调用/审批 hash，只改 SQL、参数或目标。
  - 经生产批准恢复链复核，真实工具调用计数为 0，无回执、结果行或结果引用片段，Turn 不得完成。

## 最终验证

以下命令均在上述 worktree 执行：

| 实际命令 | 结果 |
| --- | --- |
| `cargo fmt --all` | 完成 |
| `cargo fmt --all -- --check` | 通过 |
| `git diff --check` | 通过 |
| `cargo test -p fluxdb-app --locked --offline --test ai_sql_policy_boundaries` | 7 passed / 0 failed |
| `cargo test -p fluxdb-app --locked --offline` | 849 passed / 0 failed / 1 ignored；其中 lib 724 passed / 1 ignored，集成测试 125 passed |
| `cargo check --locked --offline` | 仓库编译检查通过 |
| `cargo check -p fluxdb-app --locked --offline --all-targets` | 包含测试、示例目标的检查通过 |

全量回归同时覆盖既有 tagged 参数校验、审批 CAS/拒绝/失效/仅执行一次、真实 SQLite 写审批、重启与 unknown 不重放。
审批守卫套件最终 7 passed，新增边界套件 7 passed；结果详见 `target/t024-validation/app-tests.log`。
仓库 check 保留依赖 `block v0.1.6` 的既有 future-incompatibility 警告，未调整依赖。

## 未验证与后续边界

- 未完成 T023；未在其未提交版本或合并后的版本上验证。合并时需保留本轮分类修复，重新执行相关测试。
- 未新增数据库侧只读事务/受限账号、跨库/schema 授权、驱动 deadline、参数化取消/有界读取能力。
- 函数集合并不穷尽用户自定义/扩展函数副作用；不能据本轮 AST 回归宣称数据库级安全读取。
- PostgreSQL/MySQL 函数只做方言分类验证，未在真实服务器执行；SQLite 工具回归使用真实隔离数据库文件。
- 未执行真实模型、可见窗口审批 E2E、截图或 Windows/Linux 验收；没有 UI 改动，不宣称这些项目通过。
- T024 保持未勾选，本记录仅供后续合并与完整验收使用。
