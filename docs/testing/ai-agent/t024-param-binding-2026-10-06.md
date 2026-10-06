# B：SQL 参数绑定（真实驱动 bind，拒绝字符串插值）验收（2026-10-06，macOS / podman）

## 为什么做

真实模型写 SQL 时**天然使用占位符**（PG 端到端里它第一次写请求就带了 `parameters`），
而此前执行链对非空 `parameters` 一律拒绝（`parameters_unsupported`），模型只能改写成内联字面量。
本任务把参数化执行真正接上驱动。

## 设计（对齐主设计 §20.2 / §21.4 / §20.3）

- **独立类型**：`fluxdb_core::ParameterizedQueryRequest { request: QueryRequest, parameters: Vec<SqlParameter> }`
  —— 不机械改动既有的 54 处 `QueryRequest` 构造点。
- **精确参数**：`SqlParameter::{Null,Bool,I64(String),U64(String),F64(f64),Decimal(String),Text(String),Bytes(Vec<u8>)}`；
  i64/u64/decimal 用十进制文本（不过 f64、不丢精度），bytes 由应用层 base64 解码后有界校验。
- **Connector 契约**：`execute_parameterized` 默认**明确返回 Unsupported**；
  「不支持就报错」而不是把参数拼进 SQL（插值＝注入面）。
- **应用层校验**（`ai_sql_exec::parse_sql_parameters`）：只接受 tagged wire 值
  `{type:"i64"|"u64"|"decimal"|"f64"|"text"|"bool"|"null"|"bytes", value:...}`；
  单次最多 64 项、text ≤16 KiB、bytes ≤64 KiB、decimal 只接受 `[+-]digits[.digits]`、f64 必须有限；
  非法输入返回 `invalid_parameters`，不进入执行链。

## 实现范围

| 连接器 | 状态 |
|---|---|
| **MySQL**（sqlx） | ✅ 真 bind：单语句、预编译协议 `?`，i64/u64/decimal/text/bool/f64/bytes/null |
| SQLite | ⏳ 未实现 → 默认 Unsupported（明确报错，不插值） |
| PostgreSQL | ⏳ 未实现（当前执行走 `simple_query_raw` 文本协议）→ 默认 Unsupported |

> 占位符方言转换**未实现**：调用方/模型必须使用该方言的原生占位符（MySQL `?`、PG `$n`）。

## 真实 MySQL 验收（podman `unit-mysql`，mysql:8.0.34，`127.0.0.1:38831`）

新增 `crates/fluxdb-app/examples/ai_live_mysql_params.rs`（`FLUXDB_AI_LIVE=1` 显式启用；
只写专用库 `fluxdb_ai_smoke`，结束 `DROP DATABASE`）。连接使用你已保存连接的凭据（走宿主解密路径，未外显）：

| 步骤 | 实际 |
|---|---|
| ① 连通 | `test_connection` 通过（127.0.0.1:38831） |
| ② fixture | `CREATE DATABASE/TABLE fluxdb_ai_smoke.orders` |
| ③ 参数化 INSERT | `INSERT ... VALUES (?, ?, ?)`，参数 i64/text/decimal → **affected_rows=1** |
| ④ 注入串 | 绑定 `x' OR '1'='1` 后按该值查询：命中且值原样为 `2|x' OR '1'='1` ⇒ **真 bind，不是拼接** |
| ⑤ 参数化 UPDATE | `UPDATE orders SET status=? WHERE id=?` → **affected_rows=1**；参数化 SELECT 回读为 `1|paid` |
| ⑥ 清理 | `DROP DATABASE IF EXISTS fluxdb_ai_smoke` |

## 自动化回归

| 命令 | 结果 |
|---|---|
| `ai_sql_exec_parses_all_tagged_parameter_kinds` | 通过（8 种 tagged 类型都通过解析，最终仅因该连接无 bind 而报 `parameters_unsupported`） |
| `ai_sql_exec_rejects_malformed_tagged_parameters` | 通过（未知类型/缺 value/i64 越界/`1;DROP TABLE`/u64 负数/f64 非数字/decimal 非法/base64 非法/非数组/超 64 项） |
| `cargo test -p fluxdb-core / -p fluxdb-connectors / -p fluxdb-app / -p fluxdb-desktop --offline` | 106 / **225(16 ignored)** / **699(1 ignored)**+集成 10·2·6·2·6·4·11 / 505(4 ignored) |
| `cargo check --workspace --offline` | 通过 |

## 未验证 / 未完成

- PostgreSQL 与 SQLite 的参数绑定（PG 需要把 `simple_query_raw` 换成扩展协议并做类型化解码）。
- 占位符方言转换（`?` ↔ `$n`）。
- 参数值不写入日志：本实现只在 `tracing::info!` 里记 `bind_count`，不记值（已遵守）。
- 真实窗口 GUI（本轮经服务层驱动）；Windows/Linux。
