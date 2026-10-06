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

## 未完成（因此 T025 不勾选）

- **执行器未实现**：还没有把 `redis.scan/inspect/execute/server_info` 接到 `RedisConnector`
  （需要 argv 级命令执行能力；现有 `execute_command_workbench` 是"命令文本"入口，需要新增有界的 argv 通道）。
- **审批接线**：`redis.execute` 尚未注册进目录（`include_write` 路径），因此没有真实审批卡与恢复执行。
- **真实 Redis E2E 未做**：scan → inspect → 受控 execute（含审批）与后端键值核对（你的 `docker-redis` 已在跑，下一步直接用它）。
- 大集合分段、长订阅停止、Cluster/DB scope 校验、结果分页（T027）未做。
- 平台：仅 macOS。
