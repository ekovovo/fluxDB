# C / T025 Redis 工具：风险分类与契约（2026-10-06，macOS）——**部分完成，不勾选**

## 本轮完成的部分（策略层，已测试）

新增 `crates/fluxdb-app/src/parts/ai_redis_policy.rs`：

- **四个工具定义**（设计 §22）：`redis.scan` / `redis.inspect` / `redis.server_info`（Read）、
  `redis.execute`（**Write，逐次审批**）；均 `additionalProperties:false` 且必填字段明确；
  `redis.execute` 只接受 **`argv` 数组**（不接受拼接后的命令文本）。
- **命令风险分类** `classify_redis_command(argv)`：
  - 只读白名单（GET/MGET/HGETALL/LRANGE/SCAN/TYPE/TTL/XLEN…）→ `ReadOnly`；
  - 写与脚本（SET/DEL/HSET/LPUSH/ZADD/INCR/EXPIRE/EVAL/FUNCTION…）→ `RequiresAuthorization`；
  - **管理与危险命令直接拒绝**（CONFIG/ACL/DEBUG/MONITOR/SHUTDOWN/FLUSH*/CLUSTER/CLIENT/MODULE/AUTH/SELECT）；
  - **长连接/阻塞命令拒绝**（SUBSCRIBE*/PSUBSCRIBE/MONITOR/BLPOP…）：Agent 调用不得挂成长订阅；
  - **未知命令 fail closed → RequiresAuthorization**（宁可多问一次，也不放过未识别的副作用）；
  - 空 argv → Rejected。

测试（5 例全过）：只读白名单、写/脚本需授权、管理与长连接拒绝、未知命令 fail closed、
工具定义与 schema 契约（含 `ToolCatalog::prepare` 能接受合法参数、拒绝未知工具与缺 argv）。

## 连接器侧 argv 执行原语（本轮新增并在真实 Redis 验证）

`RedisConnector::execute_argv(database, argv)`：**单条 argv** 执行（不做命令文本解析、不做 pipeline），
复用 Workbench 的受控边界（长连接/阻塞类命令由连接器拒绝）。

真实 Redis（podman `docker-redis` 6379，连接 id=3，隔离配置库副本，凭据未外显）实测
（`examples/ai_live_redis_argv.rs`，只读写 `fluxdb:ai:smoke:*` 并删除）：

| 命令 | 结果 |
|---|---|
| `PING` | `status:PONG` |
| `SET/GET fluxdb:ai:smoke:kv` | 写入成功；`GET` 返回 `hello`（无回显密钥） |
| `SCAN 0 MATCH fluxdb:ai:smoke:* COUNT 10` | 游标 `0` + 命中该键 |
| `TTL` | `int:-1` |
| `SUBSCRIBE` / `MONITOR` | **连接器拒绝**（`Unsupported`） |
| `CONFIG GET` / `FLUSHALL` / `BLPOP` | **策略层拒绝**（`Rejected{blocked_command:…}`） |
| `DEL` | `int:1`（测试键已清理） |

分层结论（重要）：连接器只挡"长连接/阻塞"这类**协议层**命令；`CONFIG/ACL/FLUSH*/BLPOP` 等业务分级由
`ai_redis_policy::classify_redis_command` 负责，因此**四个 Redis 工具必须先分类再执行**，不能直接调 `execute_argv`。

## 未完成（因此 T025 不勾选）

- **工具执行器未实现**：连接器 argv 原语已就绪（见上），但 `redis.scan/inspect/execute/server_info`
  还没有实现成 `AiTool`、也没有注册进 `standard_redis_catalog`。
- **审批接线**：`redis.execute` 尚未注册进目录（`include_write` 路径），因此没有真实审批卡与恢复执行。
- **真实 Redis E2E 未做**：scan → inspect → 受控 execute（含审批）与后端键值核对（你的 `docker-redis` 已在跑，下一步直接用它）。
- 大集合分段、长订阅停止、Cluster/DB scope 校验、结果分页（T027）未做。
- 平台：仅 macOS。
