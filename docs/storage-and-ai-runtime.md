# 存储与 AI 运行时约束

这是根目录 [AGENTS.md](../AGENTS.md) 的专项细则。修改存储、凭据或 AI 管理实体前阅读存储章节；修改 AI 执行、事件、取消、并发或运行时依赖前阅读运行时章节。与任务无关时无需通读。

## 存储结构与凭据

### 普通持久化

- `crates/fluxdb-storage/` 负责配置与连接持久化。连接配置不保存明文密码，只保存 `credential_ref` 或非敏感选项。
- `connections` / `history` 是真表，实现见 [connection_store.rs](../crates/fluxdb-storage/src/parts/connection_store.rs) 和 [history_store.rs](../crates/fluxdb-storage/src/parts/history_store.rs)。只有跨数据库类型通用的字段建列，类型专属字段进 `extra_json`；新增数据库类型不改变表结构。
- settings、sidebar layout、ER 视图、备份记录等其余设置类数据仍通过 kv 表的 load-all / save-all JSON blob 存取。AI 管理实体是例外，不把该例外扩展到普通设置。

### AI 管理实体

沿用既有设计 §42 的对象级管理，不再整体塞进 kv 的 `ai.config.v1`：

- AI 模型、Agent、插件实例/绑定、MCP 设置、运行保护策略及运行时设置分别使用 `ai_model_profiles`、`ai_agents`、`ai_plugin_instances`、`ai_agent_plugin_bindings`、`ai_budget_policies`、`ai_mcp_settings`、`ai_runtime_settings`。
- Agent 提示词直接编辑，不另设模板或多版本实体。运行保护策略沿用预算策略表名，但不包含金额或累计 token 上限。
- 每个实体独立 `revision`，使用对象级 CAS；通用身份、引用、版本和状态建列，Provider/插件专属参数放进受应用层校验的 JSON。
- DDL 沿用设计 §42.10，实际结构的唯一来源为 [schema.rs](../crates/fluxdb-storage/src/parts/ai_platform/schema.rs)；修改前核对实现，不额外维护一套竞争的结构定义。

### API Key 与非敏感视图

- 模型 API Key 不得明文落盘。AES-256-GCM 密文及元数据 `nonce`、`cipher_version`、`key_id`、`secret_revision` 保存在对应 `ai_model_profiles` 行的加密列。
- 复用连接凭据同一把数据密钥，即 `config.toml` 中的 `secret_key`；数据密钥不进入模型表，不使用独立 `ai_credentials` 表作为模型凭据的正常存储。
- 普通配置 DTO、聚合视图、导出、日志、事件和运行快照不得包含凭据明文或 ciphertext/nonce/tag 加密载荷；不得通过调试输出或盲目派生 `Debug`/`Serialize` 绕过。
- UI/Runtime 仅取得非敏感模型配置及 `NotRequired` / `Missing` / `Configured` / `Unavailable` 凭据状态。保存 API Key 使用显式 Keep/Set/Clear，禁止将明文或密文附带到普通设置保存路径。
- 连接密码、history、非 AI kv 与通用 Settings TOML 的既有规则不变；模型存储例外不能改变其他凭据的存储方式。

## AI 运行时与执行器

- `crates/fluxdb-app/src/parts/ai_*` 的异步执行器归 [ai_executor.rs](../crates/fluxdb-app/src/parts/ai_executor.rs)。`block_on` 必须依靠真实 waker park/unpark 线程，按取消和上限收尾；不使用固定轮询次数假装完成，不依赖外部 IO 反应堆。
- 取消与超时覆盖所有 await 点：等待期间 `CancelFlag` 置位、达到 wait 上限或 provider/transport 自身 deadline 时都必须尽快返回。执行器在后台线程运行，任何等待不得阻塞 UI 主线程。
- 模型增量、工具执行中进度、审批与回执在产生点即时写入有界回桥通道，UI 线程并发消费；不先攒进 `Vec` 等工具结束后回放。
- 所有通道与进度/事件回桥均有容量上限，溢出显式拒绝或按既定合并策略处理；禁止无界增长、静默丢弃终态。
- 编排复用既有 AI runtime actor 线程，不在热路径按任务新建线程。确需并发时使用固定数量后台 worker，并保证 worker 数量与队列有界。
- 生产依赖只从 `Cargo.lock` 已有版本选择，例如 `futures` / `smol`；先核查锁文件再决定直接依赖和 feature。不引入第二个自主 Agent 循环，不在本仓库外另加平台专属运行时。
