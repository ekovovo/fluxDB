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

## 四个工具实现 + 真实 Redis 验收（本轮）

新增 `crates/fluxdb-app/src/parts/ai_redis_exec.rs`：`AiRedisScanTool` / `AiRedisInspectTool` /
`AiRedisServerInfoTool` / `AiRedisExecuteTool`，以及 `standard_redis_catalog(resolver, include_write)`
（`include_write=false` 时连 `redis.execute` 的 spec 一起去掉，避免"可见但没接审批"）。
`AiRedisTarget` 解析 `conn:<id>/db:<n>`（n 限 0-15）；回包经 `redis_reply_to_json` 有界投影；
`server_info` 默认只取 server/memory/clients 且输出截断 16KiB；`inspect` 只对 string 直接取值，
其它类型给出"按需读取"提示，避免一次拉全集合。

真实 Redis 验收（podman `docker-redis`，连接 id=3，隔离配置库副本，`examples/ai_live_redis_tools.rs`）：

| 工具 | 结果 |
|---|---|
| `redis.execute SET` | Succeeded，回包 `OK` |
| `redis.inspect`（view=value） | `type=string, ttl=-1, value="hello"` |
| `redis.scan`（pattern=`fluxdb:ai:smoke:*`） | cursor `0` + 命中测试键 |
| `redis.server_info`（server） | Succeeded，输出 845 字节 |
| `redis.execute CONFIG/MONITOR/SUBSCRIBE` | **Failed `redis_command_rejected`**（策略层拒绝，长连接类连接器也会拒） |
| `DEL` 清理 | Succeeded，测试键已删除 |

## 未完成（因此 T025 不勾选）

- **未接运行时目录**：四个工具已实现并有真实 Redis 验收，但桌面运行时仍只装 SQL 目录；
  `redis.execute` 的**审批卡与恢复执行**还没在真实窗口走过（审批机制本身在 SQL 路径已验证）。
- **审批接线**：`redis.execute` 尚未注册进目录（`include_write` 路径），因此没有真实审批卡与恢复执行。
- **真实 Redis E2E 未做**：scan → inspect → 受控 execute（含审批）与后端键值核对（你的 `docker-redis` 已在跑，下一步直接用它）。
- 大集合分段、长订阅停止、Cluster/DB scope 校验、结果分页（T027）未做。
- 平台：仅 macOS。
