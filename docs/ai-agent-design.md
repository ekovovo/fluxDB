# fluxDB 原生 AI Agent 与插件平台详细设计

- 状态：设计草案；本文件不是“功能已经实现”的说明。
- 日期：2026-10-03。
- 本文件为完整设计唯一入口；工具/MCP、内置插件、设置/UI、持久化细节均在对应章节，不需要另读专项文件。
- 配套任务清单：[ai-agent-tasks.md](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-tasks.md)。
- **UI 交互稿（实现时必须对照）**：[ai-agent-ui.html](/Users/shining3d/fusuwei/code/fluxDB/docs/prototype/ai-agent-ui.html)
  - 样式 `docs/prototype/ai-agent-ui.css`、页面 `ai-agent-ui-pages.js`、交互 `ai-agent-ui.js`；浏览器直接打开即可，无需构建与网络。
  - 覆盖：数据库工作台、询问AI（抽屉/内嵌、公开思考摘要、执行时间线、**内联审批卡片**、报表产物卡片）、大模型（含Key掩码/眼睛、上下文窗口/最大输出/计价）、Agent主页面（列表+三分区）、内置插件、工具、本地MCP、安全审计（只读）、会话历史、报表弹框、高级费用/执行保护。
  - 交互稿为模拟数据：模型/数据库/MCP/附件执行均为假动作，不实现加密。原生GPUI实现以本文文字契约为准，视觉与交互以交互稿为基准；两者冲突时先改稿再改实现。
- 范围：应用内部 Agent、模型与凭据管理、插件、工具、上下文、BI 产物、独立表持久化、界面与端到端验证。
- 本地源码基线：fluxDB `d90ef97546cf342e5a6f3397798be3625373b38d`；Pi `a276dabe57911253350bffb93cb7d7aff6a73261`；Goose `591edd47cf2cfea4957d720c607cf2a4def8673d`。
- 外部网页和主分支资料为本日参考，不代表可直接使用的发布版 API；引入依赖前必须锁版本并验证。全文“新增”“拟建”“建议”均指待实现内容。


## 阅读索引（主文内）

- [Jev自动装配](#builtin-jev-activation) · [内置插件目录](#builtin-plugin-catalog) · [第三方接口边界](#extension-reserved)
- [工具目录与映射](#mcp-tool-catalog) · [MySQL/关系型工具](#mysql-tools) · [Redis工具](#redis-tools)
- [本地MCP实现](#mcp-server-implementation) · [Server授权](#mcp-server-grants) · [Claude CLI测试](#claude-mcp-test)
- [抽屉/内嵌与宽度](#ai-panel-presentation) · [打开/新建/会话恢复](#ai-panel-session-open)
- [设置菜单](#ai-settings-menus) · [时间线视觉规格](#ai-native-timeline) · [公开思考展示](#public-reasoning)
- [会话/消息parts](#session-message-parts) · [流式持久化/恢复](#session-streaming-restore) · [必经审计Hook](#security-audit-hook)
- [当前Docker连接测试](#docker-connections) · [GUI/CLI联合验收](#mcp-ui-e2e)
- 独立表DDL：§42.10/§42.11；实施顺序见配套任务清单。

## 1. 已确认的用户需求

1. Agent 原生集成到 fluxDB；不把任务交给外部 Codex、Claude Code、Goose、OpenCode 进程，也不依赖它们的桌面产品。
2. 不做只有提示词和意图分支的聊天框。Agent 能自主探索、规划、调用工具、观察错误、修正、验证、交付产物。
3. 首批支持 SQL 编写/解释/执行、数据库探索、Redis 操作、文件读取、BI 分析和报表；预留 MongoDB、HTTP API、SSH 的能力扩展。
4. 支持 `@` 对象引用、当前界面上下文、主动读取更多上下文；不能把整个库或界面内容无条件发送给模型。
5. 插件可提供模型路由、上下文提供/筛选/压缩、生命周期 Hook、工具、报表及其他输出能力。
6. 可以在执行不同阶段选用经济或高能力模型；用户指定、能力匹配、隐私范围、预算优先于插件建议。
7. Jev 是自动可用的内置可选插件，配置启用合格决策模型后自动建立系统绑定，无需安装/手工建实例。完全未配置、禁用、超时、报错、返回无效时，都不阻断正常 Agent 运行。
8. AI 管理实体必须独立建表，不用 kv 保存整个 AI 配置；普通业务 kv 和连接/history 现状不被顺手改造。
9. 模型 API Key 使用连接凭据同一把数据密钥、AES-256-GCM 加密；密文保存在模型行中。不保存到系统钥匙串，不建 `ai_credentials` 作为正常模型凭据存储。
10. 设计具体到代码职责、接口、表、约束、首次初始化、测试；参考不限于本地 Pi/Goose。
11. 测试必须真实经过界面，逐步操作、逐步断言、压缩小截图，并逐步形成上下文检查点；不一次提交大量大图片。
12. fluxDB 提供对外的本地 MCP Server，Claude Code 等外部 Agent 可调用同一套 MySQL/Redis 工具；这不是把 fluxDB 内部 Agent 外包。MCP Client 接入别人的工具仅为未来可选扩展。
13. 本期仅内置插件，第三方安装/市场/Wasm暂不实现，只留能力契约与加载来源接口。
14. 设置Sidebar新增“大模型”“工具与MCP”；Agent会话/公开思考摘要/工具卡/审计/产物持久化并有美观的原生时间线。
15. 当前已配置Docker MySQL/PostgreSQL/Redis由用户授权用于测试，复用连接与AES凭据，测试只修改本run namespace，不反复要求配置新连接。
16. 无旧AI数据，本期不做数据迁移/AI导入/迁移引擎；已有连接密钥兼容仍保留。工具前安全审计Hook为必经能力。
17. “询问AI”首次默认抽屉，之后恢复上次抽屉/内嵌形式，面板内可切换；最大宽分别60%/40%，有最小宽和独立宽度记忆；提供新建/关闭，普通打开恢复上次会话。

## 2. 产品目标与验收标准

**平台验收分工（2026-10-05）**：本设计对应的全部实施任务完成后，Windows/Linux 的启动、功能、UI、截图与 E2E 最终验收由用户自行执行，不纳入单项任务或代理交付的验收门槛，不因尚未执行而阻塞任务勾选。实施阶段仍须保证跨平台兼容，并完成当前开发平台（macOS）的相关测试与真实窗口验收；不得将单平台结果宣称为其他平台已通过。此规则统一覆盖本文与任务清单的所有任务及历史验收记录。

| 目标 | 可观察验收 |
|---|---|
| 自主数据库探索 | 未给表名时，通过工具找出相关表/列/关系，而非捏造名称 |
| 可恢复的执行 | 字段错误、工具失败等反馈进入下一轮；能修正或说明无法继续 |
| 原生交互 | 对话、`@`、审批、停止、结果/图表、报告在 fluxDB 内完成 |
| 独立于 Jev | 无 Jev 的标准回归集全部通过；有 Jev 故障不挂起主任务 |
| 安全自主 | 已授权有界读取自动执行；写入/越权/高风险外部操作由内核校验 |
| 插件化 | 同一内核可卸载/替换路由、压缩、报表插件，不重写主循环 |
| 可追溯 BI | 图表、数字能追溯到结果集、SQL、时间和指标口径 |
| 数据与凭据正确 | 对象级 CAS；失败回滚；配置/日志/导出/截图没有 API Key 或加密载荷 |
| 多平台 | 保持跨平台兼容；实施阶段验证当前开发平台，其他平台按上述最终验收分工执行 |

第一阶段不承诺任意复杂任务都成功。“智能”通过同模型、同工具、同任务的结果评测验证，不通过功能数、计划长度或会话文字判断。

## 3. 非目标及禁止做法

- 不接入外部 Agent daemon/CLI 作为主执行者；允许调用远程或本地**模型服务**，这是模型部署选择，不是外部 Agent。
- 不复制 Pi 的 TypeScript/TUI 或 Goose 的完整产品、配置和会话数据库。
- 不默认引入 OpenSpec/Superpowers；本设计为普通仓库 Markdown。
- 不实现第三方插件安装/市场/Wasm，本期不引入其运行依赖；只留扩展接口。
- 不实现旧AI迁移/导入/升级引擎。
- 不第一版就建立多个自治专家 Agent、任意代码执行、全库向量化或不受约束的 shell。
- 不把 MCP 当成智能引擎；MCP 是外部能力协议，可选且不影响原生 SQL/Redis 工具。
- 不用 SQL 首词/模型分类/工具的 `readOnlyHint` 代替实际权限校验。
- 不让插件读 API Key，不把密文列带入普通 DTO，不把“用户 @ 了对象”当成写入授权。

## 4. 现有代码与改造边界

以下均是本次实际检查的已有文件；后文列出的新增路径不是已存在功能。

| 已有位置 | 当前能力 | 复用/改造方式 |
|---|---|---|
| [state.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/state.rs) | `AppState`、`AppCommand`、`AppEvent`；SQL/Redis 执行状态 | 新增独立 AI 状态文件；不继续堆入 2600+ 行文件 |
| [controller.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/controller.rs) | include 式 controller 组装 | 只增加声明/胶水，保持现有 include 作用域 |
| [query_completion.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/controller/query_completion.rs) | `execute_query_text_for_scope_with_progress`、进度和取消回调 | 提取显式作用域执行服务；已有方法 `session_id=None`，不能直接满足 Agent 多步事务 |
| [connector.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/connector.rs) | `Connector::execute_with_progress`、元数据与取消能力 | 保持 Connector 边界；按实际数据库能力补读事务/取消/会话，不声称现有默认方法均支持强取消 |
| [redis_commands.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/redis_commands.rs) | Redis 命令规格/参数信息 | 复用解析；另建基于命令实际语义的风险判定 |
| [connection_secrets.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/connection_secrets.rs) | SQLite、AES-256-GCM、12 字节 nonce、AAD、错误密钥拒绝覆盖 | 提取共享加密原语；保留连接旧 AAD 和表格式 |
| [sqlite.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/sqlite.rs) | WAL、建表入口、按次打开 Connection | 增加 AI schema 调用、FK 和 busy timeout；不保存 AI kv blob |
| [storage lib.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/lib.rs) | `FileStorage`、平台目录、设置保存和权限收紧 | 新逻辑放 parts；现有设置保存仅保留 `connection_secret_key`，需补共享密钥兼容 |
| [dispatch_and_messages.rs](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/navicat_main/dispatch_and_messages.rs) | 主窗口 `show_message` | AI 轻量反馈统一复用 |
| [ui-style.md](/Users/shining3d/fusuwei/code/fluxDB/docs/ui-style.md) | 组件和样式规则 | 全部 AI 页面遵守 |

结构拆分与行为修改分开提交。超过 1200 行且需要增加行为的旧文件，先拆出清晰职责；不顺手重构整个状态系统。

## 5. 参考项目：采用哪些思想，明确不采用什么

### 5.1 本地源码参考

- **Pi：小内核、应用消息与模型消息分离、逐轮准备、工具前后 Hook、steering。**
  - [agent-loop.ts](/Users/shining3d/fusuwei/code/pi/packages/agent/src/agent-loop.ts)：`runLoop`、`prepareNextTurn`/`prepareRequest`、`streamAssistantResponse`、`prepareToolCall`；截断的工具调用不执行。
  - [types.ts](/Users/shining3d/fusuwei/code/pi/packages/agent/src/types.ts)：`AgentLoopConfig`、`AgentEvent`、`AgentToolResult`；模型内容和应用详情分离。
  - [compaction.ts](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/compaction/compaction.ts)：token 估算、保留近期内容、压缩切点、摘要失败检查。
  - [extensions/types.ts](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/extensions/types.ts)、[runner.ts](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/extensions/runner.ts)：扩展契约和事件执行。
  - [typesafe-system-one.ts](/Users/shining3d/fusuwei/code/pi/packages/ai/src/api/typesafe-system-one.ts)、[system-one-shared.ts](/Users/shining3d/fusuwei/code/pi/packages/ai/src/api/system-one-shared.ts)：Jev/System One 的协议分离、返回校验、取消/超时/usage。
  - 不直接运行 TS 内核；不照搬项目消息存储或默认权限策略。
- **Goose：Interface / Agent / Extensions 分层、工具检查链、取消和上下文管理。**
  - [agent.rs](/Users/shining3d/fusuwei/code/goose/crates/goose/src/agents/agent.rs)：运行编排，供定点研究，不作为复制目标。
  - [tool_inspection.rs](/Users/shining3d/fusuwei/code/goose/crates/goose/src/tool_inspection.rs)：检查结果与 Allow/Deny/RequireApproval。
  - [permission_inspector.rs](/Users/shining3d/fusuwei/code/goose/crates/goose/src/permission/permission_inspector.rs)：审批流程参考；fluxDB 不信任外部只读标注。
  - [context_mgmt/mod.rs](/Users/shining3d/fusuwei/code/goose/crates/goose/src/context_mgmt/mod.rs)：工具调用配对、压缩、上下文投影。
  - [session_manager.rs](/Users/shining3d/fusuwei/code/goose/crates/goose/src/session/session_manager.rs)：会话、消息、压缩持久化边界。
  - [retry.rs](/Users/shining3d/fusuwei/code/goose/crates/goose/src/agents/retry.rs)：重试编排参考；不照搬失败后任意 shell，不允许不确定写入重放。

### 5.2 远程项目与官方资料参考

这些项目没有假定本地存在。下列是入口级或文档级参考；实现任务需要锁定所读源码 commit，不能编造未核对的函数路径。

| 项目/资料 | 借鉴思想 | fluxDB 落点 / 不照搬 |
|---|---|---|
| [Codex App Server](https://developers.openai.com/codex/app-server/)；[core 源码入口](https://github.com/openai/codex/tree/main/codex-rs/core/src) | session/turn/item、流式事件、审批与恢复分离 | 使用自己的 AppCommand/AppEvent；不连接其 App Server，不把其 UI 协议原样搬进领域模型 |
| [Cline 文件上下文](https://docs.cline.bot/core-workflows/working-with-files)；[仓库](https://github.com/cline/cline) | 显式附件与 Agent 主动读取互补 | `@` 扩展到连接/表/结果集/Redis；不依赖 VS Code API |
| [OpenCode 插件](https://opencode.ai/docs/plugins/)、[Agents](https://opencode.ai/docs/agents/)、[插件源码](https://github.com/anomalyco/opencode/blob/dev/packages/plugin/src/index.ts) | 插件事件、工具前后 Hook、Agent 配置 | 使用类型化能力和确定的冲突规则；不运行第三方 JS，也不允许 Hook 伪造成功回执 |
| [Deep Agents](https://docs.langchain.com/oss/python/deepagents/overview)；[源码入口](https://github.com/langchain-ai/deepagents) | 按需 Skills、上下文卸载、专业能力组合 | 应用层上下文/工具/产物插件；首版不引入 Python 和默认多 Agent |
| [LangGraph 持久执行](https://docs.langchain.com/oss/python/langgraph/durable-execution)；[仓库](https://github.com/langchain-ai/langgraph) | 检查点、暂停/恢复与外部副作用边界 | 独立运行表与安全恢复；不能因为 checkpoint 就宣称跨数据库 exactly-once |
| [DBHub](https://github.com/bytebase/dbhub) | 少量探索工具、渐进发现、查询与元数据能力 | 不把整库结构塞提示词；风险校验用真实驱动能力，不只 SQL 字符串检查 |
| [Rig](https://github.com/0xPlaygrounds/rig)；[Agent 库源码](https://github.com/0xPlaygrounds/rig/tree/main/crates/rig-agent) | Rust 库、模型适配、类型化 Hook | 作为 Provider 层/基础组件候选；不把产品状态绑定到其当前 main 的 API |
| [Claude Code 工作方式](https://code.claude.com/docs/en/how-claude-code-works)；[公开仓库](https://github.com/anthropics/claude-code) | 上下文→行动→验证的动态循环 | 仅行为/官方文档参考；不是可完整复用的开源内核 |
| [vLLM Semantic Router](https://github.com/vllm-project/semantic-router) | 信号、决策、路由算法和策略拆分 | 作为路由插件结构参考，不部署网关，不使用分类结果直接授权工具 |
| [TypeSafe Jev 文档](https://docs.typesafe.ai/) | 结构化决策与概率 | 可选 DecisionProvider；不是生成式主模型或权限裁判 |
| [Extism Rust Host](https://extism.org/docs/quickstart/host-quickstart/)、[Manifest](https://extism.org/docs/concepts/manifest/) | Wasm 宿主函数、网络/文件/内存约束 | 仅未来加载参考；本期不引入、不做沙箱原型 |

[Vanna](https://github.com/vanna-ai/vanna) 仅参考分析到图表的产品路径；其公开仓库已归档，不选为核心依赖。任何源码移植单独检查所选 commit 的 LICENSE/NOTICE、兼容性与归属，不把“公开可见”当成“可任意复制”。

<a id="tool-mcp-source-reference"></a>

### 5.3 数据库工具与MCP的开源做法

| 参考 | 实际机制/入口 | 采用 / 不采用 |
|---|---|---|
| [DBHub](https://github.com/bytebase/dbhub)，[tools 源码目录](https://github.com/bytebase/dbhub/tree/main/src/tools) | 默认少量 SQL/对象发现工具，支持自定义和渐进探索 | 学少工具、渐进结构和输出边界；fluxDB 将 read 与 mutate 分开，避免只用一项 execute_sql 的提示来证明安全 |
| [Redis 官方 MCP](https://github.com/redis/mcp-redis)，[源码入口](https://github.com/redis/mcp-redis/tree/main/src) | 核心按 String/Hash/List/Set/ZSet/Stream 等数据类型提供工具，也有 CLI 风格入口和可选能力 | 学类型适配和命令能力覆盖；fluxDB 汇聚为少量组合工具，不把所有命令 schema 一次发给模型，不默认开放任意 CLI 命令 |
| [MCP Toolbox for Databases](https://github.com/googleapis/genai-toolbox) | sources/tools/toolsets 分层、配置化 SQL/参数工具 | 学连接、工具、工具集分离和业务参数工具；不引入独立外部执行内核，不把凭据塞配置文本 |
| [官方 Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) | rmcp、ServerHandler、typed tools、stdio transport | 复用协议/生命周期/schema 设施，不手写整套 JSON-RPC |
| [Goose Server runner](/Users/shining3d/fusuwei/code/goose/crates/goose-mcp/src/mcp_server_runner.rs) | `server.serve(stdio()).await`、等待关闭 | 本地 Rust 运行入口参考；不依赖 Goose 产品 |
| [Goose tutorial Server](/Users/shining3d/fusuwei/code/goose/crates/goose-mcp/src/tutorial/mod.rs) | Parameters/JsonSchema、tool_router/tool_handler、CallToolResult | typed handler 和返回结构参考；不能未经授权直接注册全部工具 |
| [MCP transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)、[tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools) | stdio 生命周期、结构化工具结果、错误/提示元数据 | 版本协商，stdout 只用作协议；annotations 不是权限证明 |
| [Claude Code MCP](https://code.claude.com/docs/en/mcp)、[CLI](https://code.claude.com/docs/en/cli-reference) | stdio Server 配置及非交互客户端运行 | 使用隔离 `--mcp-config` 实测，不修改全局配置、不运行 Claude 自己的 MCP Server 来冒充 fluxDB Server |

以上不是“某项目最佳”的排名。工具拆分采用能力+风险边界，再通过同模型任务实测验证 discoverability 与调用成本。

<a id="source-reference-policy"></a>

### 5.4 参考代码的索引与就近引用规则

总索引 §5 保留用于找项目与版本；**每个实现章节末尾放就近参考**，指出读取哪个类/函数、借鉴机制、fluxDB落点和差异；任务也关联相应章节/参考。不是每句话都放重复链接，也不是只有开头一张表。

具体实现前重新核对远程commit；无法核实的文件不编造路径。本本文件的本地链接已实际存在；公开主分支入口不等于已锁定依赖API。本轮不采用未核实的Cline具体源码路径，Cline仅保留已核对的官方上下文文档/仓库入口参考。

## 6. 技术选择与依赖策略

**采用应用内部 Rust runtime，借鉴 Pi/Goose；不直接依赖整个 Agent 产品。**优先验证 Rig 等可嵌入 Rust 库能否提供可步进循环/流式 Provider，并在工具执行前交还宿主控制。T003 通过则复用其循环或适配能力；只有无法满足审批、真实回执、取消和恢复契约时，才实现薄循环并记录缺口。不能套两个自主循环重复执行同一工具，也不把自有数据库/插件契约绑定到 SDK 私有状态。模型 HTTP、SSE、重试等成熟基础能力优先复用标准库/现有 SDK，不为“自己 Agent”重复造轮子。

优先复用已安装 serde、serde_json、tracing、rusqlite、aes-gcm、rand、sqlparser、futures/tokio 版本。应用层当前 tokio 是 dev-dependency，不能当成生产可用依赖。拟新增生产 async HTTP/取消/JSON Schema/UUID/摘要/敏感内存清零能力时，先核查锁文件，再决定直接依赖和 feature；不在本设计里虚构版本。

HTTP 流式调用要可取消、有大小限制、有 connect/read deadline；现有 ureq 不自动视为足够的异步 SSE 实现。新 async 网络逻辑与同步 Connector 必须隔离在后台执行器。

## 7. 分层与建议代码落点

以下路径为**拟新增**，每个目录按职责拆小文件；原有 lib/main 只加声明。

- 领域 DTO：[core/ai/mod.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/ai/mod.rs)，下分 model、agent、message、context、tool、plugin、artifact、budget、error；不依赖 GPUI/HTTP/数据库驱动。
- 编排：[app/ai/mod.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/ai/mod.rs)，下分 runtime/{actor,loop,state,event}、provider/{mod,chat,system_one,scripted}、context/{assemble,compact,mentions}、tools/{registry,db,redis,file,artifact}、plugins/{registry,hook,route,builtin}、policy/{permission,budget,privacy}、management/{models,agents,plugins,mcp}。
- 持久化：[storage/ai_platform/mod.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/ai_platform/mod.rs)，下分 schema、init、models、agents、plugins、runtime、sessions、messages、parts、ui_state、audits、usage、credentials；密文行只在此层。
- 共享加密：[secret_crypto.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/secret_crypto.rs)，复用连接密钥，不在 app/UI 实现 AES。
- UI：[main_parts/ai/mod.rs](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/ai/mod.rs)，下分 panel、composer、mentions、timeline、reasoning、tool_card、session_list、approval、artifacts、settings、models、agents、plugins、mcp；普通控件复用 gpui-component 0.6.0。
- E2E：[scripts/ai-e2e/](/Users/shining3d/fusuwei/code/fluxDB/scripts/ai-e2e)，[docs/testing/ai-agent/](/Users/shining3d/fusuwei/code/fluxDB/docs/testing/ai-agent)。真实执行时才产生证据目录；本轮不造截图。

不立刻新建独立 crate：先在 app 的职责目录建立边界；若 runtime 成为独立可复用库，再单独拆 crate，不随设计扩建仓库。

## 8. 核心数据类型与接口

代码块是接口草案，不是当前可编译补丁。实现时应补全 imports、具体错误、ID 新类型和测试。只对稳定多实现能力使用 trait；运行时异构 Provider/工具/插件采用 `dyn`，不为纯函数和单一内部服务制造接口。

```rust
pub type AiFuture<'a, T> = std::pin::Pin<Box<
    dyn std::future::Future<Output = AiResult<T>> + Send + 'a
>>;

pub trait GenerationProvider: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities;
    fn stream(&self, request: ModelRequest, cancel: CancellationToken)
        -> AiFuture<'_, ModelEventStream>;
}

pub trait DecisionProvider: Send + Sync {
    fn decide(&self, request: DecisionRequest, cancel: CancellationToken)
        -> AiFuture<'_, DecisionResponse>;
}

pub trait AiTool: Send + Sync {
    fn descriptor(&self) -> &ToolDescriptor;
    fn execute(&self, ctx: ToolExecutionContext, args: ValidatedArguments)
        -> AiFuture<'_, ToolReceipt>;
}

pub enum CredentialStatus { NotRequired, Missing, Configured, Unavailable }
pub enum ApiKeyChange { Keep, Set(SecretInput), Clear }
```

`ModelRequest` 不带 API Key；Provider transport 的私有调用边界临时向凭据服务申请 lease。`SecretInput` 不实现 Serialize/Display，Debug 仅 `[REDACTED]`；Set 明文仅通过专用保存命令进入 storage，不进普通状态、事件、历史、请求录制。普通模型 DTO 仅含非敏感配置和 CredentialStatus；明文Key只在用户点击眼睛后短暂存在于编辑框内存，不进DTO。

应用层的 AiRepository trait 属于 app，storage 不得反向依赖 app。具体 FileAiRepository 适配器放 app，持有 FileStorage 并调用 storage 的 AI repository 方法；通用 DTO 位于 core。UI 只派发命令，不持有解密服务。测试通过构造函数注入 mock，不用线程局部测试凭据 override 驱动跨线程生产任务。

`ToolReceipt` 分为不可变的真实执行回执、面向模型的有限投影、面向 UI 的结果/产物引用。Hook 不得改写执行是否成功、影响行数或审批事实。

**就近参考：**[Pi AgentLoopConfig/事件契约](/Users/shining3d/fusuwei/code/pi/packages/agent/src/types.ts)；[当前AppCommand/AppEvent](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/state.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 9. 会话、用户轮次与模型迭代

- Session：用户与某 Agent 的长期会话。
- Turn：一次用户任务和其中的补充/审批，拥有固定目标与配置快照。
- Iteration：一次主模型推理；一个 Turn 可有多次 iteration。
- ToolCall：iteration 内一个工具调用，拥有稳定 call ID 和真实执行状态。
- ModelRequest：主模型、决策、摘要、报表生成各自的调用，均计量费用。

会话存储不能等同于模型 messages。应用消息保留 `@`、审批、产物、上下文版本等；只在 Provider 边界投影为其支持的消息格式。切换 Provider 时不搬运私有推理字段/隐藏状态；工具调用与结果必须配对且完整。

**就近参考：**[Pi session entries/checkpoint](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/session-manager.ts)；[Goose会话与消息表](/Users/shining3d/fusuwei/code/goose/crates/goose/src/session/session_manager.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 10. 主循环与自主性

```text
接收用户目标→持久化 Turn/固定目标和配置→运行时校验
→装配小上下文→按需压缩→模型路由→预算预留
→调用主模型并流式显示→校验完整工具请求
→工具 Hook→再校验参数/风险→审批（如需）→执行/持久化回执
→观察结果/错误→吸收用户补充→下一 iteration
→有依据地交付，或暂停请求必要信息，或明确失败/取消
```

### 10.1 内核控制流的 Rust 草图

下面展示控制责任而不是要求抄写所有 helper；若复用 SDK，把对应边界接到这些应用服务。公共上下文与秘密 transport 保持隔离。

```rust
async fn run_turn(&mut self, id: TurnId) -> AiResult<TurnOutcome> {
    let mut turn = self.repo.load_running_turn(id).await?;
    loop {
        self.check_cancel_and_limits(&turn)?;
        self.apply_queued_steering(&mut turn).await?;
        let draft = self.context.assemble(&turn).await?;
        // 默认路由始终可用；可选决策插件返回的是建议。
        let model = self.router.select_authorized(&turn, &draft).await?;
        let context = self.context.fit_and_validate(draft, &model, &turn).await?;
        let request = self.build_request(&turn, model, context)?;
        // tracked service 在所有成功/错误/取消路径结算 actual 或 unknown usage。
        let response = self.call_model_tracked(request, &turn).await?;
        self.repo.append_assistant(&turn, &response).await?;
        if response.tool_calls.is_empty() {
            return self.finish_or_wait_user(turn, response).await;
        }
        if response.is_truncated() {
            self.append_unexecuted_call_errors(&turn, &response).await?;
            self.advance_iteration(&mut turn).await?;
            continue;
        }
        for call in response.tool_calls {
            self.check_cancel_and_limits(&turn)?;
            // 内部：初校验→可选Hook→重新校验→强制安全审计→prepared/audit同事务→审批→执行。
            // 普通工具错误返回 receipt 给模型；基础设施失败才退出整个任务。
            let receipt = self.execute_checked(&turn, call).await?;
            self.repo.append_tool_receipt(&turn, &receipt).await?;
            self.events.publish_receipt(&turn, &receipt).await?;
            if receipt.outcome.is_unknown_write() {
                return Ok(TurnOutcome::NeedsReconciliation);
            }
        }
        self.advance_iteration(&mut turn).await?;
    }
}
```

actor 外层无论该函数正常/错误/取消都执行状态收尾与 usage/reservation 清理；不能用 `?` 退出后让 Turn 永远 generating。有限并行读取是后续优化，首先以顺序执行保证审批、结果配对和清晰取消。

模型可自行选择搜索结构、读取文件、查询数据、补充分析和产物工具；不预写 SQL→BI 的固定步骤。每轮无进展的相同工具/参数/相同目标版本重复超过阈值时，返回明确提示允许调整，之后安全暂停；不能只因 SQL 相同就拒绝合理验证。

模型 stream 未结束或 finish reason 为截断时，不执行可能不完整的工具参数。参数不是合法 JSON/schema 时返回结构化错误，允许模型修正。不能用 json 修复器猜出危险参数。

**就近参考：**[Pi runLoop/prepareToolCall](/Users/shining3d/fusuwei/code/pi/packages/agent/src/agent-loop.ts)。实际落点按本章/§7，不照搬源项目私有状态。

## 11. 状态机、取消与恢复

Turn：`queued → preparing → generating → executing_tools ↔ waiting_approval/waiting_user → completed`；可进入 `cancel_requested`、`cancelled`、`failed`、`interrupted`、`outcome_unknown`。`outcome_unknown` 不是普通可重试失败。

- 单 Session 一个活动 Turn；用户补充通过有序 steering 队列，在下一安全边界吸收。
- 用户“停止”立刻阻止新请求/新工具；给正在运行的网络和 Connector 传取消。服务端操作能否取消由能力说明决定。
- 不能用停止 UI 转圈证明 SQL 未执行；不支持取消的写操作继续核实，显示“已停止后续步骤，当前操作结果待确认”。
- 重启后纯读取可由用户继续；等待审批重新校验资源版本并重新展示；未确认写操作不自动重放。
- 不保证分布式 exactly-once。SQLite 保存执行意图与远端提交之间存在崩溃窗口，必须用状态核实/幂等键/人工确认处理。

**就近参考：**[Goose执行/取消编排](/Users/shining3d/fusuwei/code/goose/crates/goose/src/agents/agent.rs)；[当前带scope查询服务](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/controller/query_completion.rs)。实际落点按本章/§7，不照搬源项目私有状态。

### 11.1 T029 启动恢复的实现边界（2026-10-06）

当前实现只处理**本地恢复与禁止自动重放**，不在本阶段注册写工具或发真实模型请求：

- 存储入口：`crates/fluxdb-storage/src/parts/ai_platform/recovery.rs::recover_on_startup`。使用 SQLite `IMMEDIATE` 短事务，在宿主启动且尚无任务执行时扫描；失败整批回滚。不能在普通 `Load`、切会话或打开面板时重复扫描活跃任务。
- 应用入口：`AiConversationCommand::RecoverOnStartup`，实现为 `crates/fluxdb-app/src/parts/ai_recovery.rs`。桌面首帧在 `main_parts/navicat_main/render.rs` 排入 AI IO 串行队列，后台执行；失败保留错误并禁用发送，不能装作空会话。
- `queued/preparing/generating/executing_tools/cancel_requested` 未完成轮次：无未知副作用则 `interrupted`；专用只读工具的 `running` 记为读取中断（call=`cancelled`、不伪造 receipt），写/未知工具的 `running` 则 call=`unknown`、Turn=`outcome_unknown`。已有终态成功回执不改写。`prepared` 只表示未执行，结束为 cancelled，不重放。
- 未执行待审批项：pending 保留；历史 approved 退回 pending 并递增 revision，卡片同步新的 `approval_revision`，旧 UI 的 CAS 决定失效；过期批准不可使用。片段/审批事实不一致时 fail closed，不重新开放陈旧按钮。
- 正文、公开摘要、草稿、展开选项、原目标和滚动锚点保留；未完成流式片段标 interrupted。补一条稳定 ID 的恢复 notice，重复启动扫描不会重复建卡或增加相同 revision。未知执行在工具卡显示“结果未知”，与普通“已停止”区分。
- 默认内存结果在重启后 expired；retained 结果不在本扫描中删除或假设重建。结果分页、文件校验与空间管理仍属 T027。
- 审批决定、调用状态和原卡片更新在同事务完成。恢复执行在另一短事务中重新检查当前 Agent 权限/审计、规范化参数、工具版本、固定目标和 plan hash，再把 waiting_approval 占用为 running 并消费批准，事务外才执行。已有 running/unknown/终态或同连接存在未核实未知副作用时拒绝重复/冲突写入。
- 已拒绝的重启任务立即撤下操作按钮并更新恢复提示，不继续显示“等待批准”。未知结果没有“一键重试写操作”或假装核实成功的入口；需先核对真实数据库/外部系统。

就近代码：`ai_platform/tool_calls.rs`（保留 sessions re-export，事实行 API），`main_parts/ai/actions.rs`/`panel.rs`（启动排队、恢复提示）、`crates/fluxdb-app/tests/ai_restart_recovery.rs` 与 `tests/support/ai_restart_recovery_approval.rs`（故障窗口、幂等、CAS、防重放）、`main_parts/ai/tests_recovery.rs`（生产加载→AppState→控件）。

**本节不宣称**完整 T029 已验收：跨系统提交结果核实/幂等键、连接资源 revision 的完整授权复核、真正恢复模型工具消息配对及关闭窗口时的未知写收尾仍须按任务清单继续完成；不因本地扫描通过而勾选全部 T029。

## 12. 运行时、AppCommand 与 AppEvent

拟新增命令：`StartAiTurn`、`SendAiSteering`、`CancelAiTurn`、`ResolveAiApproval`、`ResumeAiTurn`、`LoadAiSessionPage`；管理命令按实体分开，如 `SaveAiModelProfile`、`SaveAiAgent`、`SaveAiAgent`（包含system_prompt）、`SaveAiPluginBinding`。

专用 `SaveAiModelProfile` 包含 ApiKeyChange，其 Set 参数不持久化为命令日志。所有含敏感输入的命令自定义 Debug，不使用自动 derive 泄漏。

拟新增事件：Turn 状态、文字 delta、工具开始/进度/回执、审批请求、上下文压缩、模型选择、usage 更新、产物完成、脱敏错误。事件携带 session/turn/request/call ID 和递增 seq；旧 Turn 的迟到事件不修改新 Turn 状态。

Runtime 使用后台 actor 管理任务，控制消息与取消通过有界通道；UI 主线程只 dispatch、更新 AppState、render。不可持有全局 AppController/SQLite mutex 跨网络 await。SQLite 同步 IO 和 Connector 阻塞调用交给有界后台线程池。

文字 delta 可 30–60ms 合并更新 UI；最终消息、审批与执行回执不得丢弃。数据库不为每个字符写一行事件，定期保存文本 checkpoint 和最终消息。

**就近参考：**[Pi AgentEvent/steering](/Users/shining3d/fusuwei/code/pi/packages/agent/src/types.ts)；[当前AppState/事件](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/state.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 13. 自动界面上下文与目标绑定（无需用户确认）

用户不选择连接/目标。Turn 开始时内核**自动从当前活动标签捕获**上下文并固定执行目标，Agent 直接开干；只有当捕获内容本身不可用（如未连接、标签已关闭）才在面板内给一次性说明，而不是弹确认框打断任务。

### 13.1 捕获规则

| 当前活动标签 | 自动捕获 | 目标（TargetRef）绑定 |
|---|---|---|
| SQL 编辑器 | 有选区→选区；否则全文（按§13.2限额）；光标所在语句 | 连接ID + 当前库/schema |
| 表数据 / 结果网格 | 表结构摘要、列名、当前筛选/排序、可见前若干行样本、结果集句柄 | 连接ID + 表所在库 |
| Redis Key 面板 | key、类型、TTL、当前预览片段 | 连接ID + Redis DB |
| 建表/ER 等设计页 | 对象路径与结构摘要 | 对应连接/库 |
| 无标签页 | 侧栏当前选中连接 + 最近活动对象 | 该连接 |

捕获产生 `UiContextSnapshot`（含 `tab_id/revision/target/selected_text/last_error/result_ref`）。发送时固定 TargetRef：之后切换标签页**不改变**该 Turn 的执行目标；用户显式换目标才清缓存并使旧审批失效（§31.5）。MCP 外部调用没有界面上下文，必须显式给资源句柄（§28.6），这是安全边界不是 UX 缺陷。

### 13.2 太大时怎么办

分级捕获，永不一次性塞满：

1. **选区优先**：用户选中的文本最相关，只发选区。
2. **整段限额**：SQL/文本超过阈值不全文发送。默认单段上限 **16 KiB / 400 行**（可按模型上下文调大，不硬编码超模型能力）。
3. **首尾保留**：超限时发 **头部 60% + 尾部 40%**，中间用 `… [省略 N 行，可用 editor.read_range 读取] …` 标记；SQL 通常头是 WITH/注释、尾是 ORDER BY/LIMIT，头尾命中率最高。
4. **可续读**：暴露 `editor.read_range {tab_ref, offset, limit}` 与 `result.read_page` 工具，模型按需拉取任意区间；工具返回同样受行/字节预算与截断标记约束。
5. **快照而非复制**：捕获存引用+区间，不把整份内容永久复制进会话；压缩时优先卸载原始区间，保留工具句柄。

参考：[Pi read 工具](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/tools/read.ts)（超限截断 + 告知 offset 续读）、Cline 官方上下文文档（有选区先给选区）、Goose 上下文管理（工具配对与渐进卸载）。区别：fluxDB 面向结构化 SQL/结果/Key 而非代码文件。

**就近参考：**Pi read 截断策略；[当前 SQL 编辑执行器](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/sql_editor_adapter/statements.rs)。实际落点 ai/context/assemble + editor 工具。

## 14. `@`、文件与对象引用

`ContextRef` 是结构化引用：类型、稳定 ID、连接与作用域、revision/fingerprint、展示标签、来源、授权范围。输入框显示 chip，不依靠模型解析字符串 `@orders`。

候选包括连接、库/schema、表/视图、保存查询、编辑器选区、结果集、Redis Key、文件、知识条目；禁止默认把候选对象正文全发给模型。失效引用给可见说明；同名表按连接+库+schema 区分。

安全审计同时拒绝将fluxDB配置/凭据SQLite作为可查询数据库资源，不能只拦file.read而通过SQL读取配置。文件授权采用canonical root + 单文件授权，Path/PathBuf、平台目录和安全打开策略；每次读取检查 symlink/目录穿越与授权。存在 TOCTOU 风险的平台优先采用受控打开句柄，无法保证时明确限制。远端 SSH 文件与本机文件是不同资源空间。

读取按 byte/range/line 有界，二进制返回类型/大小与可选预览，不按 UTF-8 硬解；CSV 流式解析，超长行、编码错误、截断均可见。不允许读取 fluxDB 的配置密钥/凭据库作为普通附件。

**就近参考：**[当前对象引用](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/object_query.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 15. 上下文装配与大小预算

优先级：系统约束与能力→用户目标/补充→固定目标/审批状态→显式 `@` 的摘要→相关结构/知识→最近完整工具交互→旧摘要。权限与预算是内核状态，不仅存在于提示词。

上下文上限按**最终选择模型**能力计算：输入预算 ≤ context_window − max_output − tool/schema/安全余量；不同模型切换后重新估算。token 估算器是可替换能力；未知 tokenizer 用保守估算并保留误差，不能宣称字符数精确等于 token。

大结果只带列结构、行数/截断标记、少量样本和 ResultRef。工具结果不得无限追加，读下一页是明确工具调用。结果内容不按“系统指令”解释，带来源与不可信标记；标记降低风险但不被描述为完全解决提示注入。

**就近参考：**[Pi context估算/切点](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/compaction/compaction.ts)。实际落点按本章/§7，不照搬源项目私有状态。

## 16. 压缩插件与重要性判断

基础策略始终存在，无 Jev 也能运行：保护目标/用户限制/业务口径/最近完整工具配对，卸载大结果，必要时由已授权生成模型摘要；摘要模型可单独配置，无配置用主模型。

ContextProcessor 返回 `Keep / Summarize / Offload / OmitFromActiveContext` 计划，不能删除原始审计记录，也不能卸载未完成的工具配对。DecisionProvider 可对相关性评分；生成式 Provider 负责摘要。

摘要检查：结束正常、未截断、必保条目完整、来源引用可解析；校验失败不覆盖旧 checkpoint。只提交成功的摘要和边界，再标记新 context generation。失败时先减少可卸载结果；仍超预算则暂停并说明，不能默默删用户限制。

频繁压缩受 min interval、成本预算和进展检查约束，避免“压缩→超限→压缩”的无穷循环。压缩结果记录保护条目 hash、source message 范围、所用模型和 usage，不保存密钥。

**就近参考：**[Pi摘要失败/保留边界](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/compaction/compaction.ts)；[Goose工具配对压缩](/Users/shining3d/fusuwei/code/goose/crates/goose/src/context_mgmt/mod.rs)。实际落点按本章/§7，不照搬源项目私有状态。

### 16.1 百分比触发与增量摘要（2026-10-06 用户修订）

Agent 配置 `compression_trigger_percent` 默认 80，范围 10–95，存在 `ai_agents.options_json` 并随 Agent revision/CAS 保存；不是 KV，也不新建提示词或策略版本表。按本轮实际生成模型的有效上下文窗口计算阈值，模型路由变化需重新计算，不以消息条数或累计账单 usage 触发。输入预算同时扣除实际最大输出与安全余量：达到百分比或输出预留空间不足时自动处理，不需审批；未配置 Jev 时完全可用。

采用 Pi 的合法切点与增量策略：旧有效摘要 + 新增旧消息生成新摘要，保留最近完整工具请求/回执、当前用户要求、目标、业务口径及待审批事实；不能拆未完成工具对。摘要结构为目标、限制、已验证事实/来源、已完成/进行中/阻碍、决策、下一步；兼容可验证 JSON/文本但不把解析失败原文当成通过安全校验的 checkpoint。保留原始消息/审计，只有完整、非截断且保护项及来源校验通过的摘要才原子提交 `ai_context_checkpoints`。取消、失败、迟到结果、无进展均不得覆盖旧 checkpoint。分离窗口上限、请求上下文用量、累计计费用量。

参考：[Pi 切点/摘要失败检查](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/compaction/compaction.ts)、[Goose 比例阈值/可见性投影](/Users/shining3d/fusuwei/code/goose/crates/goose/src/context_mgmt/mod.rs)。配置与完整运行压缩分别验收，不把保存百分比等同 T035 已完成。

**实现补记（2026-10-07）**：摘要调用接入统一 `MeteredProvider`（预留/结算，purpose=`compaction`）；摘要只在 `Finished(Stop)` 且 JSON 含全部保护项（goal/constraints/verified_facts/decisions/next_steps 且非空）时才提交；checkpoint 提交在写锁事务内重新校验 turn 活动状态、generation 连续性与 `last_message_seq` 前进，拒绝迟到/回退；已有 checkpoint 作为请求基础上下文（旧摘要 + 其覆盖 seq 之后的历史），不是只在下次压缩时才复用。

## 17. 模型配置与路由

模型分 `generation / decision` 能力，不把 Jev当作 tool-calling 主模型。Profile 包含 Provider、协议、模型 ID、endpoint、**上下文窗口Token**、**最大输出Token**、能力声明/探测、启用状态、**计价（输入/输出/缓存读取，每百万Token）**、非敏感 options、凭据状态，以及**连接状态**（`unknown`/`ok`/`failed` + 最近检测时间，属运行时元数据，不参与配置 `revision` CAS）。测试连接发送真实最小对话请求（提示词固定「你好」），按 Provider 协议走 `/v1/chat/completions` 或 `/v1/messages`。

路由 precedence：用户明确选择→内核授权/隐私/能力/预算过滤→插件候选建议→默认模型/规则。插件不得越过用户固定模型；默认固定模型完整可用，不要求先做复杂度分类。

**实现补记（2026-10-07）**：会话级「用户明确选择」落在独立真表 `ai_session_model_selections`，优先级为「会话/草稿显式选择 → Agent 绑定模型 → 全局默认模型」，草稿选择在首次发送事务内转移到会话；`New` 清空草稿选择但保留已有会话选择。停用/删除/`decision` 类模型被拒时给出可见错误并**保留选中项**，不静默替换成其它模型；活动 Turn（含 `outcome_unknown`）期间禁止切换。查询AI底部提供选择器并通过 `管理模型` 跳转设置页；设置页仍保留全局默认，二者是「默认值 + 会话级覆盖」的关系。

经济/高能力是用户可配置标签和真实价格/能力信息，不按模型名硬编码；上下文窗口与最大输出按模型真实能力配置，用于§15上下文预算计算，不允许配置超过Provider实际限制。失败升级只用于可重新生成的推理；不重放已经执行的工具。插件冲突采用明确优先级与 stable binding ID 顺序，只有一个最终路由结果；所有候选都不合格则可见失败。

所有模型步骤，包括 Jev、压缩、报表生成，进入统一 usage/budget 路径。新模型不支持当前工具/上下文时重新选择或暂停，不清空历史凑合运行。

**就近参考：**[Pi prepareNextTurn/prepareRequest](/Users/shining3d/fusuwei/code/pi/packages/agent/src/agent-loop.ts)。实际落点按本章/§7，不照搬源项目私有状态。

### 17.1 模型能力自动获取与输入单位（2026-10-06）

获取模型列表返回结构化 `id/context_window_tokens/max_output_tokens`，只解析正整数或可精确转换的数字，限定响应、候选数量与token上限，不根据名称臆测、不用聊天询问模型自身窗口。优先 Provider 当前部署元数据（Anthropic max_input_tokens/max_tokens，OpenRouter context_length/top_provider.context_length/max_completion_tokens，兼容服务可声明同名字段）；普通 OpenAI `/models` 没有窗口字段，缺失显示“未提供”，保留手填不填假值。用户手动值优先，显式获取及选择模型可填入未手改字段；Provider/endpoint/key/弹框实例变化使旧结果失效，获取期间 loading/disabled。凭据复用 AES 宿主链，认证不随redirect泄漏。

上下文/输出输入框右侧各放 `K / M` 单位选择，标签明确 tokens，默认 K（不是 KB 字节）：1K=1000 tokens，1M=1000000 tokens，支持可精确换算的小数，数据库存正整数token。切换单位保持实际数值，编辑重开与自动获取不损精度；保留已有保存下限（窗口≥1024、最大输出≥256），最大输出不能超过已知窗口。不把测试连接成功视为容量已探测。

UI参考唯一入口 [ai-agent-ui.html](/Users/shining3d/fusuwei/code/fluxDB/docs/prototype/ai-agent-ui.html)，需同步HTML原型；符合 §39.3–39.5 正常/窄窗、明暗主题、真实点击与有数据闭环。

## 18. Jev 内置插件：自动可用且完全可选

内置ID `fluxdb.decision.jev`，随应用注册，默认auto；使用通用DecisionProvider。用户只需启用该内置能力并配置可用Jev决策模型，不需要下载安装/手工建instance/binding。

启动/模型保存与状态变化/Agent创建时reconcile system-managed绑定：默认决策模型优先，否则唯一合格候选自动使用；多个候选无默认时提示选择，基础Agent继续运行，不擅自发数据或选择贵模型。默认引用在ai_runtime_settings.default_decision_model_profile_id，自动角色在ai_agent_model_bindings列，binding_source=system；用户手动绑定优先不被覆盖。

| 状态 | 行为 |
|---|---|
| 未配置/关闭/无可用模型 | 不请求、不等待；基础路由/压缩照常 |
| 合格模型已配置且插件启用 | 自动可用，建议通过内核能力/隐私/预算过滤 |
| 多候选未设默认 | 状态“待选择”，不阻断 |
| 低置信度/非法返回 | 放弃建议，使用基础策略 |
| 超时/无凭据/网络错 | 一次脱敏诊断，熔断暂跳过，不每轮等待 |

可调起始参数deadline800ms、无自动重试、连续3次失败熔断60s，不是Jev性能承诺。相关性/路由批量判断与数据最小化，费用仍记ledger。Jev不可required，不作为SQL/文件权限裁判。

完整内置能力清单/自动装配见§18.1、§23.2；[内置能力目录](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-design.md#builtin-plugin-catalog)。

<a id="builtin-jev-activation"></a>

### 18.1 内置Jev自动装配与状态规则

内置 ID：`fluxdb.decision.jev`。它使用 DecisionProvider，不是主模型；插件程序随 fluxDB 编译，用户不用安装包或手工建立 instance/binding。

启动和配置变化时统一执行 `reconcile_builtins`：

1. 注册所有内置能力，幂等建立 system-managed package/instance/binding 元数据；不写 API Key 或默认付费模型。
2. 筛选 enabled、decision 类型、支持 Choice/Score、身份可用且符合隐私授权的模型。不能只根据 model 名称含 jev 猜协议。
3. 有明确默认决策模型时使用它；只有一个合格 Jev profile 时自动使用；多个候选又无默认时显示“请选择默认决策模型”，不偷偷选贵模型或发数据。
4. 为 Agent 自动建立 system 来源的 decision role binding，模型 ID 在引用列，不藏 plugin config JSON；用户手动指定的 role binding 优先，不能被 reconcile 覆盖。
5. 已启用内置 Jev 且模型可用时运行；未配置/禁用/无凭据/协议不兼容/超时/低置信度使用基础策略。

模型保存/测试通过、启用/禁用、凭据 Set/Clear、默认决策模型改变、Agent 新建都会触发 reconcile。运行中只在下一安全边界采用新配置；当前模型请求不可无声换目标。

状态显示：未配置、已禁用、可用、待选择模型、暂不可用（附脱敏原因/熔断截止），不是一排永久错误 toast。插件可启停；不可设置 required。先前“未配置时必须手工创建插件实例”设计作废。

自动启用不代表自动允许数据出站：DecisionRequest 仅包含任务/结构摘要，仍检查该 Provider 的隐私/预算许可。无 Jev 完整运行，主模型有无配置是另一个独立问题。

**参考与落点：**[Pi System One adapter](/Users/shining3d/fusuwei/code/pi/packages/ai/src/api/typesafe-system-one.ts)、[shared validation](/Users/shining3d/fusuwei/code/pi/packages/ai/src/api/system-one-shared.ts)、[逐轮模型准备](/Users/shining3d/fusuwei/code/pi/packages/agent/src/agent-loop.ts)；[TypeSafe 文档](https://docs.typesafe.ai/)。落 app provider/system_one、plugins/builtin、management/models；本文§18、§42 的默认模型/绑定来源列。

**就近参考：**[Pi TypeSafe协议](/Users/shining3d/fusuwei/code/pi/packages/ai/src/api/typesafe-system-one.ts)；[System One返回校验](/Users/shining3d/fusuwei/code/pi/packages/ai/src/api/system-one-shared.ts)。实际落点按本章/§7，不照搬源项目私有状态。

## 19. Agent内提示词、Skills与业务知识

提示词直接在Agent的“模型与提示词”页用多行输入框编辑，保存在ai_agents.system_prompt，与该Agent一起做revision/CAS；不建Agent内提示词表，不提供发布、版本比较或独立提示词管理页面。revision用于对象并发控制，不是提示词多版本管理。

Turn开始捕获Agent revision、system_prompt原文与其hash到受隐私策略约束的运行快照；编辑只影响下一Turn，不能热替换正在执行的提示词。内核权限、预算、工具校验不依赖用户提示词，可以为空时仍有安全系统约束。原文不写入普通诊断日志，长度限64KiB UTF-8；摘要/上下文计入模型预算。

Skills 是按需加载的专业说明、工具使用原则和输出要求，不是强制固定流程；插件提供 skills 与业务知识。指标定义、时间口径、退款/取消规则和表关系必须有作用域、来源、确认状态和 revision。

首版使用结构化知识条目和检索，不依赖全库向量索引；模型提出的新规则必须用户确认才能成为长期事实。知识与上下文隔离到连接/库/Agent，不能把另一生产库的口径当作当前库真相。

**就近参考：**[Pi Skills](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/skills.ts)。实际落点按本章/§7，不照搬源项目私有状态。

### 19.1 内置默认 Agent 与自定义助手（2026-10-06 用户修订）

代码提供默认定义，首次开库幂等 seed 一条稳定 ID `fluxdb.agent.default` 到 `ai_agents`，在设置页置顶并标“内置”。用户允许修改名称、提示词、模型、工具及压缩比例；不可删除、不可停用。重启只补缺失行，不覆盖用户编辑。不绑定凭据/默认付费模型，模型未绑定则跟随全局主模型；只配置模型即可开始询问，不要求手动建 Agent。

新会话优先用户显式默认 Agent，否则内置默认；不自动选择列表第一项。已有会话维持原身份，身份不可用时给出明确错误而不偷偷替换正在运行的审批目标。自定义助手可新建（以默认定义预填）、复制、编辑、删除；删除采用对象级 CAS 与确认，有运行/待审批 turn 时拒绝，逻辑删除以保留会话、审计和外键事实并清理可编辑绑定/默认引用，列表隐藏。配置只影响下一轮，内核安全不能被提示词修改。

参考：[Pi prompt 拼装](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/system-prompt.ts)、[Goose PromptManager](/Users/shining3d/fusuwei/code/goose/crates/goose/src/agents/prompt_manager.rs)。这是默认定义加数据库可编辑持久化，不声称 Pi/Goose 有同样管理页。

### 19.2 子 Agent 的独立循环与交付边界

子 Agent 是委派产生的独立子会话，不是设置页新建的助手配置。复用 `TurnRunner`，继承经过父权限交集收窄的工具/目标和模型配置，有独立上下文及压缩；只允许一层。默认最大 25 次模型迭代，父取消联动，父仅接收有界最终结果及状态；真实子工具/审批走原审计链，审批在询问AI面板显示并标明归属，不能用“delegate工具获批”代替具体写入审批。必须记录父会话、父调用、任务身份，恢复不能重复委派或重放未知写入。没有对应实现/验收前不得显示可用开关或声称已有子 Agent 能力。

参考：[Pi durable subagent](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/experimental/durable/subagent.ts)、[Goose 子会话 reply 循环](/Users/shining3d/fusuwei/code/goose/crates/goose/src/agents/subagent_handler.rs)。后续按本次追加任务实现与验收，不用外部 pi/goose 进程替代原生运行。

## 20. 工具定义、发现与返回协议

```rust
pub struct ToolDescriptor {
    pub id: ToolId,                 // db.query、redis.scan 等稳定宿主 ID
    pub version: u32,
    pub description: String,
    pub input_schema: JsonValue,
    pub output_schema: JsonValue,
    pub capability: CapabilityKind,
    pub risk: RiskClass,
    pub effect: EffectClass,        // PureRead / Mutating / ExternalSideEffect
    pub timeout_ms: u64,
    pub output_limit_bytes: u64,
}

pub struct ToolReceipt {
    pub call_id: ToolCallId,
    pub outcome: ToolOutcome,      // Succeeded / Failed / Denied / Cancelled / Unknown
    pub model_projection: BoundedToolContent,
    pub result_ref: Option<ResultRef>,
    pub artifact_refs: Vec<ArtifactRef>,
    pub execution_facts: ExecutionFacts,
}
```

协议无任意 JSON 参数透传驱动：先 schema 校验，再业务/作用域校验。对 Provider 工具名称限制做可逆名称映射，不把点号命名假定为所有 Provider 都可接受；持久化使用稳定宿主 ID。

初始只暴露基本探索与任务相关工具。`tools.search` 返回描述和能力，不自动授权；`tools.describe` 获取具体 schema。确定可用后，在下一模型请求加入工具定义，已有工具调用绑定 version 不随插件更新漂移。

错误用 `code/message/retryability/details_ref`：权限错误不能诱导模型尝试另一个绕过工具；未知写入不是 retryable。输出有字节/行数/token 限制和 `truncated/next_cursor`，不能用截断结果宣称全集结论。

### 20.1 首批工具目录

| 工具 ID | 输入摘要 | 结果 / 约束 |
|---|---|---|
| `connections.list` | caller scope | 只列已授权连接资源；不返回 endpoint/凭据 |
| `operation.status` / `operation.cancel` | request ref | owner 权限、真实取消与 unknown 状态 |
| `redis.server_info` | target、sections | 受限版本/DB/诊断；非任意 CONFIG |
| `ui.current_context` | 无或请求的字段集合 | 最新小快照，不改变固定目标 |
| `context.resolve` | ContextRef、detail level | 受授权的对象详情 |
| `db.search_objects` | target、query、kinds、cursor、limit | 渐进对象列表；limit ≤ 100 |
| `db.describe_object` | ObjectRef | 列/索引/关系/注释与 fingerprint |
| `db.query` | target、sql、parameters、max_rows、timeout | 受控读取，ResultRef，默认最多 200 样本行 |
| `db.explain` | target、sql、mode | 默认不执行 ANALYZE；执行式计划另审批 |
| `db.execute` | target、statements/parameters | 修改专用入口，审批绑定完整 plan hash |
| `result.read_page` | ResultRef、cursor、limit | 分页；过期/已释放可见错误 |
| `redis.scan` | target、pattern、cursor、count | 渐进 SCAN；COUNT 是提示不是硬返回上限，宿主仍限字节/批量 |
| `redis.inspect` | target、key、range | TYPE/TTL/受限值；大集合分段 |
| `redis.execute` | target、argv | 命令语义风险、secret 参数隔离、审批 |
| `file.search` | 授权 root、pattern、limit | 有界路径结果，不越 symlink/目录范围 |
| `file.read` | FileRef、range、max_bytes | 分段数据、编码/截断信息 |
| `artifact.create` | renderer、title、sources、spec | 生成声明式产物草稿 |
| `artifact.export` | ArtifactRef、format、destination grant | 通过宿主安全写入，不允许插件任意路径 |

### 20.2 可执行 schema 示例

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "required": [
    "target",
    "sql"
  ],
  "properties": {
    "target": {
      "type": "string",
      "minLength": 1
    },
    "sql": {
      "type": "string",
      "minLength": 1,
      "maxLength": 65536
    },
    "parameters": {
      "type": "array",
      "maxItems": 1024,
      "default": [],
      "items": {
        "$ref": "#/$defs/parameter"
      }
    },
    "max_rows": {
      "type": "integer",
      "minimum": 1,
      "maximum": 1000
    },
    "timeout_ms": {
      "type": "integer",
      "minimum": 100,
      "maximum": 30000
    }
  },
  "$defs": {
    "parameter": {
      "oneOf": [
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type"
          ],
          "properties": {
            "type": {
              "const": "null"
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type",
            "value"
          ],
          "properties": {
            "type": {
              "const": "text"
            },
            "value": {
              "type": "string",
              "maxLength": 16384
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type",
            "value"
          ],
          "properties": {
            "type": {
              "const": "bool"
            },
            "value": {
              "type": "boolean"
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type",
            "value"
          ],
          "properties": {
            "type": {
              "const": "i64"
            },
            "value": {
              "type": "string",
              "pattern": "^-?[0-9]+$",
              "maxLength": 20
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type",
            "value"
          ],
          "properties": {
            "type": {
              "const": "u64"
            },
            "value": {
              "type": "string",
              "pattern": "^[0-9]+$",
              "maxLength": 20
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type",
            "value"
          ],
          "properties": {
            "type": {
              "const": "f64"
            },
            "value": {
              "type": "number"
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type",
            "value"
          ],
          "properties": {
            "type": {
              "const": "decimal"
            },
            "value": {
              "type": "string",
              "pattern": "^-?[0-9]+(\\.[0-9]+)?$",
              "maxLength": 256
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "type",
            "value"
          ],
          "properties": {
            "type": {
              "const": "bytes"
            },
            "value": {
              "type": "string",
              "maxLength": 21848
            }
          }
        }
      ]
    }
  }
}
```

`target` 是内核提供的资源句柄，不是模型可以提交的连接字符串。parameters 使用 tagged wire 值；i64/u64/decimal 是精确文本，bytes 是有界 base64，应用还校验实际数值范围/decimal尺度/编码后字节数；NULL 不含value。不能用浮点丢失数据库精度，也不允许任意对象进驱动；数据库方言 placeholder 转换必须明确定义和测试。

### 20.3 MySQL/Redis 的具体工具拆分与对外协议

完整工具设计见§20.4–20.6、§21、§22；[工具目录](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-design.md#mcp-tool-catalog)；关系型 5 个、Redis 4 个、公共 4 个；默认不注册每条数据库命令，也不以万能 exec 替代权限边界。对外 MCP 名称和内部 ToolId 一一映射，共用执行服务，不另写驱动执行链。

MySQL/PostgreSQL/SQLite 共用 db.* 契约，具体 dialect/capability 在 Connector 实现；Redis 以 scan/inspect/execute/server_info 划分，inspect 根据类型选择有界读取。MCP Profile 根据资源类型和读写授权过滤 tools/list，隐藏工具也不得直接调用绕过。

当前 QueryRequest 没有参数字段：参数化请求新增独立 wrapper/Connector 能力，驱动真实 bind，拒绝字符串插值。§20.2与§21.4使用同一tagged精确参数schema，schema/version 必须一致，不能同时接受不同语义的 number。Redis 通用 RESP bulk 上限 512MB 不够，AI/MCP 路径在 decode 前使用小预算，不能仅截断最终响应。

<a id="tool-split"></a>

### 20.4 一个Server、能力拆分与共享实现

采用一个 `fluxdb-mcp` stdio Server 暴露已授权资源，服务内按能力拆工具：

- **关系型数据库 5 个工具**：搜索对象、描述对象、查询、查询计划、修改执行。MySQL/PostgreSQL/SQLite 共用契约，适配器决定 dialect/能力，不复制 mysql_query/pg_query/sqlite_query 三组工具。
- **Redis 4 个工具**：扫描 Key、查看 Key、受控命令、服务诊断。不为 GET/HGET/LRANGE 等每个命令暴露一个工具，也不把整个 Redis API 塞进万能工具。
- **公共 4 个工具**：列已授权连接资源、读结果页、查操作状态、取消操作。
- 共 13 个可能公开的工具；实际 `tools/list` 按 Server Profile、授权资源类型和风险过滤。只读 Profile 不暴露两种修改工具，MySQL-only Profile 不暴露 Redis 工具。

“一个 Server”不等于“一个工具”；部署简单与权限/语义清晰可以同时成立。以后可为某 Profile 只挂 MySQL 或 Redis，但不另外复制执行实现。

<a id="mcp-tool-catalog"></a>

### 20.5 13个对外MCP工具及内部映射

| MCP name | 内部 stable ID | 首版职责 | 默认风险 |
|---|---|---|---|
| `fluxdb_connections_list` | `connections.list` | 列 Profile 已授权资源/作用域/能力，不返回 endpoint 或凭据 | read |
| `fluxdb_search_objects` | `db.search_objects` | 搜表、视图和字段线索，渐进 metadata | read |
| `fluxdb_describe_object` | `db.describe_object` | 列/索引/关系/注释/可选 DDL | read |
| `fluxdb_query` | `db.query` | 单条受控只读查询，绑定参数，有界结果 | read policy |
| `fluxdb_explain` | `db.explain` | 不执行式查询计划 | read policy |
| `fluxdb_execute` | `db.execute` | 单条修改，受预授权/审批约束 | write |
| `fluxdb_redis_scan` | `redis.scan` | 有界、可继续的 Key 探索 | read |
| `fluxdb_redis_inspect` | `redis.inspect` | TYPE/TTL 与按类型分段值预览 | read |
| `fluxdb_redis_execute` | `redis.execute` | 受控 Redis 命令，禁止认证/危险管理旁路 | write policy |
| `fluxdb_redis_server_info` | `redis.server_info` | 授权范围的版本/当前 DB/受控诊断 | read |
| `fluxdb_result_page` | `result.read_page` | 读已有结果快照，不重新执行 SQL | read |
| `fluxdb_operation_status` | `operation.status` | 查当前 caller 有权看的 request 状态/未知结果 | read |
| `fluxdb_operation_cancel` | `operation.cancel` | 取消 caller 自己的进行中请求 | control |

外部名称采用普通下划线方便 CLI 精确授权；内部工具不重复建第二实现。annotations 仅提示 readOnly/destructive/idempotent，不用来做实际授权。MCP 不默认暴露 ui.current_context/file.read/artifact.export/内部模型管理；这些需要额外用户授权和已实现能力，不能因内部存在就全部公开。

<a id="tool-result-contract"></a>

### 20.6 资源句柄、精确值与结果协议

`connections.list` 返回 `target` 句柄、label、kind、数据库/schema 或 Redis DB、read_only、可用工具、资源版本；只列授权条目。示意：

```json
{
  "resources": [
    {"target":"resource-mysql-sales","label":"测试销售库","kind":"mysql","database":"fluxdb_mcp_fixture","read_only":true},
    {"target":"resource-redis-cache","label":"测试缓存","kind":"redis","database":"0","read_only":true}
  ]
}
```

句柄在 server session 中绑定 grant ID/revision、连接配置 revision、scope，不是模型随意提交的 URL/数据库名。只读 SQL 中出现别的库/schema、Redis SELECT/跨节点目标变化，仍经资源权限判定。授权资源被修改/禁用/连接消失后返回 STALE_RESOURCE/RESOURCE_UNAVAILABLE，不静默绑定另一个库。

MCP result 使用 object 顶层 structuredContent，并带可读 text JSON 兼容投影；错误 isError=true，含 code/message/request_id/outcome，不能以“success text”掩盖失败。与 SDK 协商的客户端不支持某字段时保留等价 text 投影，不硬编码最新 protocolVersion。

```json
{
  "ok":true,
  "request_id":"operation-handle",
  "outcome":"succeeded",
  "data":{
    "columns":[{"name":"channel","type":"text"},{"name":"revenue","type":"decimal"}],
    "rows":[[{"type":"text","value":"web"},{"type":"decimal","value":"120.50"}]],
    "returned_rows":1,"total_rows":null,"truncated":false,
    "result_ref":"result-handle","next_cursor":null
  },
  "warnings":[]
}
```

默认响应软上限 64KiB、硬上限 128KiB，可按 Profile 收窄；行默认≤200、最大≤1000，单元格预览默认≤4KiB，列数/元数据/诊断均限额。超限标明 truncated/omitted，不能只在序列化后砍掉 JSON。大整数/decimal/binary 使用有标签文本/引用，不能经浮点或 JS number 丢精度。

结果句柄绑定 owner/profile/session/授权 scope；另一 session 不可用旧 ResultRef 取数据。metadata 持久化不表示结果永久可取；会话结束 memory result 过期。分页是已取得快照的切片，不重复 SQL，也不声称 SELECT LIMIT 得到了全表 total_rows。

**就近参考：**[当前Connector能力](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/connector.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 21. SQL 执行、安全读取与事务

读取与修改入口分开，但最终均经同一个权限服务。`db.query` 不能仅因 SQL 以 SELECT/WITH 开头就放行：CTE 可修改数据，函数可有副作用，SELECT INTO/锁定/导出/存储过程也可能越权。sqlparser 用数据库对应 dialect；未知/解析失败按高风险处理，不自动执行。

纵深约束：连接只读配置、宿主 AST 策略、数据库权限/只读事务、资源限额。PostgreSQL/SQLite/MySQL 分别验证真实只读执行和取消能力；某驱动不能保证时，不展示“安全自动读”能力，要求受限只读账号或显式审批该风险。不将客户端提示当成服务器强制权限。

Agent 使用明确 `ExecutionScope` 和专属 `QuerySessionId`，不借用用户编辑器事务。默认每次读取有界独立事务；只有用户任务明确需要时才支持多步事务，并展示事务状态、提供 rollback、设置空闲超时。数据库不支持统一回滚的 DDL/批量操作不能伪装原子性。

复用应用层查询服务和 history/rollback 能力；模型 SQL 原文、最终执行 SQL、参数摘要、分页/限制改写分别记录关联。新增 `ExecutionSource::Ai` 必须先核对现有 CommandExecutionSource/history 语义，不塞字符串字段绕过类型。

远端执行前持久化 prepared call/审批 hash，执行后保存回执。写入崩溃窗口无法靠本地事务消除；默认 `OutcomeUnknown`，通过核实工具/管理员确认继续。没有确认前，不自动重试 UPDATE/INSERT/DDL。

<a id="mysql-tools"></a>

### 21.1 对象搜索：`fluxdb_search_objects`

输入：target、search（≤256 字符）、kinds（table/view/column）、cursor、limit（默认30、最大100）、detail（summary）。元数据按授权数据库过滤，分页定位不是全库 DDL dump。

输出：对象引用、kind、库/名称、命中字段摘要、fingerprint、next_cursor。大小写遵循服务器 lower_case_table_names/实际名字，不硬编码小写。视图/表同名与字段位置明确区分。

复用 [mysql/objects.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/mysql/objects.rs)、[completion.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/mysql/completion.rs) 和 Connector metadata 接口；信息查询 SQL 放 connectors，不从 UI 拼 SQL。

### 21.2 对象描述：`fluxdb_describe_object`

输入：target、object_ref、include（columns/indexes/foreign_keys/comment/ddl），DDL 非默认；输出真实列名/类型/nullable/default/主键、索引/关系/注释与 freshness/fingerprint。不采样行数据，采样需要 db.query。

复用 [metadata.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/mysql/metadata.rs) 与 `Connector::list_indexes/table_ddl/list_completion_columns...`；字段/DDL 大时受限返回 detail ref，不截断后当完整 schema 使用。

### 21.3 查询：`fluxdb_query`

输入：target、sql、parameters、max_rows、timeout_ms。只允许一条只读语句；拒绝多语句、事务控制、设置、DML/DDL、SELECT INTO OUTFILE、LOAD_FILE、SLEEP/BENCHMARK/GET_LOCK 等越界或副作用能力。函数/未知语法不靠模型猜安全，read role/readonly transaction 与 host 策略共同约束。

SQL placeholders 是 MySQL `?`；参数数目、类型、size 验证，驱动 bind，不拼接值。对业务列标识符不能用 value parameter，应来源于真实对象并由 dialect quoting 处理。max_rows 是返回预算，不是对数据库计算/扫描成本的完整保证，须真实 server/driver deadline 和权限。

请求示例：

```json
{
  "target":"resource-mysql-sales",
  "sql":"SELECT channel, SUM(amount) AS revenue FROM orders WHERE status = ? GROUP BY channel",
  "parameters":[{"type":"text","value":"paid"}],
  "max_rows":100,
  "timeout_ms":5000
}
```

实际执行 options 显式 `continue_on_error=false/split_statements=false/page_offset=0`，AST必须恰好一条语句；不能直接复用 QueryExecutionOptions 的默认多语句继续行为。

[shared_read_exec.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/shared_read_exec.rs) 当前 MySQL 结果使用 fetch_all 后再转 page，单设 page_size 不约束预分页内存。AI/MCP 新路径必须改为受控流式取得最多 max_rows+1 和累计字节，不先fetch全量。单个巨大cell仍可能在SQLx解码时占大内存：需要已验证的driver/packet预算或安全有界投影；若无法做到，该大字段读取能力不能宣称硬内存有界，自动路径明确拒绝或要求专门预览工具。不能用序列化后的128KiB证明解码峰值安全。

工具不自动猜订单口径或改 WHERE 条件。时间范围/退款排除由 Agent 查知识/问用户后形成 SQL，并在报告注明。

### 21.4 当前代码缺口：真正的参数化执行

[QueryRequest](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/object_query.rs) 当前只有 text/options/scope/session_id，没有参数字段；不能仅在工具 schema 声明 parameters 然后忽略，也不能字符串替换。

建议新增 `ParameterizedQueryRequest { request: QueryRequest, parameters: Vec<SqlParameter> }` 和 Connector 的参数化执行能力，默认返回 Unsupported；不一次机械改动全部现有 QueryRequest literal。先为 MySQL 实现 sqlx bind，后续 PG/SQLite 对应实现；无参数的已有服务可复用。公共 trait 属于 core，具体绑定在 connectors。

```rust
pub enum SqlParameter {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),              // 必须 finite，不能装 decimal
    Decimal(String),       // 校验后转驱动精确类型
    Text(String),
    Bytes(Vec<u8>),         // 外部只接受有界 base64/tagged input
}

fn bind_mysql_params<'q>(sql: &'q str, params: &[SqlParameter]) -> ConnectorResult<BoundQuery<'q>> {
    // 草图：query(sql)→逐项 bind；返回具体 SQLx Query/Arguments 类型。
    // Null 需明确型别策略，decimal 不转 f64；Unsupported 类型返回明确错误。
    // 不把参数写入日志，也不将值格式化回 SQL 字符串。
    build_mysql_query_with_bound_arguments(sql, params)
}
```

外部参数采用 tagged JSON，i64/u64/decimal 的 value 是十进制文本；输出从 CellValue/实际列类型投影，不能把精确数值先转 F64 再声明 decimal。新的 tools/query DTO 和本文§20.2 同步演进，schema/version 验证防混用。

### 21.5 查询计划：`fluxdb_explain`

输入：target、sql、parameters；默认 plain EXPLAIN，不开放执行式 ANALYZE。结果实际 plan/成本估计/使用索引/行估计，标记 estimate 不作为实际行数。引用基准 SQL；模型可自主比较索引/改写建议，但不自动创建索引。

MySQL 版本不支持某 explain 格式时返回能力说明或受支持的 plain plan，不硬编码假定 MySQL 8/TiDB 一样。授权仍检查原始 SQL 访问对象，不能通过 EXPLAIN 绕过库范围。

### 21.6 修改：`fluxdb_execute`

输入：target、single sql、parameters、operation intent；修改单独工具，默认未授权 Profile 不列出/不能直接调用。首版不支持多语句 script 或跨调用隐式 BEGIN/COMMIT，不与用户编辑器事务串用。

允许范围由 Profile+resource grant 给定（操作类型、对象、明确参数/条件范围、事务能力），不能用 prompt “用户同意”作为批准。DDL、无 WHERE、跨库、高风险管理命令不能通过普通 scoped write grant 放行；需要实际桌面审批能力或返回 APPROVAL_REQUIRED。

`affected_rows` 的真正硬上限需要事务/锁定与提交前验证；不能在 UPDATE 已提交后检查行数再声称安全。首版 scoped write 只选能可靠 preflight/rollback 的 DML 类型，不保证 MySQL DDL 原子回滚。未知执行结果为 unknown，绝不自动重试。

**就近参考：**[当前MySQL执行/取消](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/shared_read_exec.rs)；[应用scope服务](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/controller/query_completion.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 22. Redis 操作的语义安全

默认探索用 SCAN/HSCAN/SSCAN/ZSCAN 和范围读取；不使用 KEYS、全量集合/大字符串返回或无限订阅。SCAN COUNT 不能限制服务器精确返回条数，宿主追加字节/总 key/时间上限，返回 cursor 和截断信息。

风险判定包括命令、子命令、参数、Redis topology/DB 和实际版本；禁止只使用命令首词作为只读判定。DEL/UNLINK、FLUSHDB/FLUSHALL、CONFIG、ACL、脚本、模块、迁移等按能力和高风险策略处理；Lua/function 默认不允许自动执行。Cluster 不能假定支持 SELECT 或单节点全集扫描；返回扫描范围说明。

Pub/Sub、Stream 等长任务有明确订阅时限、消息数/字节上限和 stop handle。取消结果与 SQL 同样不能伪造服务端已撤销。

<a id="redis-tools"></a>

### 22.1 扫描：`fluxdb_redis_scan`

输入：target、pattern、cursor、count_hint、max_keys；固定 Redis DB，key prefix grant 是强制过滤边界，不能把 pattern 当授权。默认 count_hint=100/max_keys=100，硬上限由 Profile 控制。

输出：key 引用、有限 TYPE/TTL 摘要（按需）、next_cursor、complete、truncated、扫描节点/范围。SCAN 可重复、不是一致快照，客户端去重；Cluster 返回节点级 opaque cursor，不能把单节点 cursor 称为全 Cluster 进度。COUNT 不保证回复量。

### 22.2 查看：`fluxdb_redis_inspect`

输入：target、key_ref、view（metadata/value）、cursor/range、limit、max_bytes；host 根据真实 TYPE 调用受控方法，而不是让模型决定先 GET 还是 HGETALL。

| 类型 | 有界策略 | 返回补充信息 |
|---|---|---|
| string | STRLEN + GETRANGE，避免全量 GET | 总字节、offset、encoding、truncated |
| hash | HSCAN/指定 field；每批限额、RESP 解码预算 | cursor，字段与大小/截断 |
| list | LRANGE 有界索引 | start/end、实际数量 |
| set | SSCAN | cursor，非稳定顺序说明 |
| zset | ZSCAN/有界 range | member/score，排序/遍历方式 |
| stream | XRANGE/XREVRANGE 有界 COUNT | entry ID/fields、next position |
| 模块/未知类型 | metadata + unsupported_detail | 不盲目执行可能巨大 JSON.GET |

TYPE 与随后读取可能被并发写改变；返回非一致快照标记，类型不匹配做一次受限重新检查或明确 race，不无限重试。TTL=-1 是永久、-2 是不存在，不能误作秒数；binary key/value 使用受限编码/引用，不强行 lossy UTF-8。

复用 [RedisConnector](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/redis/connector.rs) 的 load_string_value/load_hash_fields/load_set_members/load_list_items/load_zset_members/load_stream_entries；[string.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/redis/string.rs) 已有 GETRANGE 预览，但模块 JSON 不能假定同样有界。

### 22.3 现有 RESP 解码需要更小的 AI/MCP 预算

[util.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/redis/util.rs) 当前合法 bulk 上限是 512MB，数组逐项增长；这是通用 Redis 客户端保护，不足以成为 Agent 小结果预算。只在 tool result 截断已经来不及防解码内存峰值。

拟增加 `RedisReadBudget { max_bulk_bytes, max_total_bytes, max_elements, max_depth }`，对 AI/MCP 路径配置小预算，例如 bulk≤64KiB、total≤256KiB、elements≤1000，所有数字可收窄。在读长度/循环元素之前检查并累计实际解码预算；遇到预算超限中止并关闭/丢弃连接，不把未消费 RESP 数据的连接送回池。桌面既有全量下载保留原行为/明确大值流程，不全局硬改成 64KiB。

批量 HSCAN/SSCAN 不能控制每个 value 大小；超限给 VALUE_TOO_LARGE 与可行下一步，不在内存已经分配后假装只返回64KiB就安全。可按需提供宿主固定审查过的只读 range helper，但不开放模型任意 Lua 来规避预算。

### 22.4 命令操作：`fluxdb_redis_execute`

输入：target、argv（数组）、intent。argv 直接参数化协议编码，不拼接 shell/CLI string。自动能力限定明确命令/子命令/参数；所有 key 从命令 key spec/明确解析获得，再验证 prefix/DB scope。不能只检查 argv[1]，MSET/DEL/EVAL/动态 key 参数各不同。

默认禁止 AUTH、HELLO 中的 AUTH、SELECT、ACL、CONFIG、MODULE、MIGRATE、FLUSH*、SHUTDOWN、任意 EVAL/FCALL 等认证/范围切换/管理入口；并不是只要 Profile allow_write 就全部可执行。SET/HSET/DEL/EXPIRE 等也需要写范围和参数大小校验。TTL 更新和其他不可回滚动作不宣称事务可撤销。

基础 raw argv 是受控底座，未来常用业务可由插件提供更强 typed set/delete/expiry 工具；不每一个命令都做默认工具声明。RESP 编码、Cluster key slot/路由、取消与 unknown 同共用 Connector。

### 22.5 服务诊断：`fluxdb_redis_server_info`

输入：target、sections；默认版本、当前 DB 状态，其他全局内存/运行统计需 info 能力授权。过滤敏感网络拓扑/配置/其他 DB 信息；不把 CONFIG/ACL 或 SLOWLOG 正文当一般 server info。输出小摘要和数据时间。

**就近参考：**[Redis类型能力](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/redis/connector.rs)；[RESP解码预算缺口](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/redis/util.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 23. 插件实例、能力注册与绑定

区分三个对象：

1. Package：插件代码/manifest、版本、digest 和安装状态。
2. Instance：同一插件的一份独立配置，例如“报表-销售”和“报表-运营”。
3. Binding：某 Agent 使用某 Instance 的哪些能力、顺序和错误策略。

内置插件和可安装插件遵循同一声明式契约；实例不等于代码包。数据库独立存 instance 和 binding，模型引用通过独立角色绑定，知识 scope/模板等类型专属参数受 schema 校验，不包含凭据。

能力接口是稳定的单职责边界：`ModelRouter`、`ContextProvider`、`ContextProcessor`、`AgentHook`、`ToolProvider`、`ArtifactProducer`。插件可注册多个能力；实现时不要让每个简单回调都变成一个 trait。运行时 registry 汇总已授权能力，公共字段由 core DTO 表达，执行 trait 归 app。

### 23.1 稳定能力接口草图

```rust
pub trait ModelRouter: Send + Sync {
    fn advise(&self, ctx: RouteContext) -> AiFuture<'_, RouteAdvice>;
}
pub trait ContextProcessor: Send + Sync {
    fn plan(&self, ctx: ContextPlanInput) -> AiFuture<'_, ContextPlan>;
}
pub trait AgentHook: Send + Sync {
    fn on_event(&self, event: HookInput) -> AiFuture<'_, HookDecision>;
}
pub trait ArtifactProducer: Send + Sync {
    fn describe(&self) -> &ArtifactProducerDescriptor;
    fn build(&self, request: ArtifactBuildRequest, host: ScopedArtifactHost)
        -> AiFuture<'_, ArtifactBuildResult>;
}

pub enum HookDecision {
    Observe,
    Patch(AllowedHookPatch),
    Deny { reason: String },
    RequireApproval { reason: String },
}
```

RouteContext 只含经内核初步筛选的候选和受限任务摘要；RouteAdvice 只引用候选 ID，不能提供新 endpoint/凭据。ContextPlan 包含条目 ID 和处理方式，不返回任意替代系统约束。AllowedHookPatch 按事件分别定义字段白名单；不能使用任意 serde_json merge 覆盖 runtime。

ScopedArtifactHost 仅提供结果分页、来源校验、受控文件创建/提交和宿主模型请求；没有 raw filesystem、Connection 或 SecretCrypto。宿主函数对每次调用重新核对权限/取消/预算，不把注册时检查当成永久授权。

加载顺序：验证 package→确认信任/授予能力→校验 instance config schema→解析 binding→注册 registry→开始新 Turn。运行中的 Turn 绑定版本/digest；更新或卸载需等活动调用结束或用户取消，不能用新代码恢复旧调用。

<a id="builtin-plugin-catalog"></a>

### 23.2 内置能力目录与启用边界

| 内置能力 ID | 负责内容 | 启用方式 / 不允许关闭的边界 |
|---|---|---|
| `fluxdb.context.ui` | 当前页面、@对象摘要和主动细节读取 | 默认开；数据出站仍检查权限 |
| `fluxdb.tools.database` | 关系型探索、SQL、Redis 标准工具 | 默认注册；每 Agent/工具/连接可收窄 |
| `fluxdb.context.files` | 附件、授权目录、有界文件读取 | 按授权开启，不默认全盘 |
| `fluxdb.model.router` | 默认模型/经济与高能力候选路由 | 默认开；用户固定模型优先，关闭优化仍有主模型基线 |
| `fluxdb.decision.jev` | 可选分类、路由/相关性建议 | 自动可用，允许关闭，不阻断 |
| `fluxdb.context.compaction` | 保护条目、卸载、摘要、checkpoint | 默认开；优化关闭不移除输入上限和保护约束 |
| `fluxdb.security.audit` | 调用前确定性风险/授权检查和审计 | 强制注册，不能由配置删除/跳过 |
| `fluxdb.usage.budget` | usage、预算预留、费用统计事件 | 限额执行属于内核；统计展示可调整 |
| `fluxdb.knowledge.skills` | 已确认业务知识、按需专业说明 | 可开关/配置 scope，不把提议当事实 |
| `fluxdb.output.bi` | 图表、表格、报告创建和渲染说明 | 可开关/绑定模板，控制输出 |
| `fluxdb.progress.plan` | 公开计划、工具进展、检查点说明 | 默认开；只显示真实事件/模型公开内容 |
| `fluxdb.lifecycle.telemetry` | 脱敏阶段耗时/错误/诊断汇总 | 可开关；关键执行/审计落盘不依赖它 |

不把“插件化”解释成能卸掉全部保护：预算、权限、取消、真实回执和已确认副作用是内核不变量。内置安全审计使用相同 Hook 接口便于扩展，但 该Hook的执行必经，用户只能在允许范围内收窄/增强规则。

UI 显示系统实例，普通用户不需要先配置12个插件；高级页只编辑可选能力和参数。当前没有市场/安装/上传第三方代码操作。

**参考与落点：**[OpenCode plugin API](https://github.com/anomalyco/opencode/blob/dev/packages/plugin/src/index.ts)、[Pi types](/Users/shining3d/fusuwei/code/pi/packages/agent/src/types.ts)、[Goose tool inspection](/Users/shining3d/fusuwei/code/goose/crates/goose/src/tool_inspection.rs)；落 app ai/plugins/builtin 与 capability registry。

**就近参考：**[Pi扩展事件/能力](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/extensions/types.ts)；[Pi扩展调度](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/extensions/runner.ts)。实际落点按本章/§7，不照搬源项目私有状态。

## 24. Manifest 与配置协议示例

以下是 fluxDB 的拟定格式，不是 Codex 插件格式，也不是最终 Extism manifest：

```json
{
  "schema_version": 1,
  "id": "fluxdb.bi.report",
  "version": "1.0.0",
  "host_api": {"min": 1, "max": 1},
  "runtime": "builtin",
  "capabilities": ["tool_provider", "artifact_producer"],
  "permissions": ["result.read", "artifact.create", "artifact.export.request"],
  "config_schema": {
    "type": "object", "additionalProperties": false,
    "properties": {"theme": {"enum": ["system", "light", "dark"]}}
  },
  "entrypoints": {"describe": "describe", "invoke": "invoke"}
}
```

模型引用统一保存在 ai_agent_model_bindings，插件配置只选用其已注册角色，不将模型 ID 列表藏在普通 config_json；MCP 授权引用保存在 ai_agent_mcp_bindings。实例配置和 secret bindings 分离；manifest 标明 secret 字段类型，普通 JSON 中只能出现 SecretBindingRef。内核阻止把 api_key/password/token/Authorization 等敏感字段写入普通配置；字段黑名单仅辅助，不能替代 schema 与专用秘密输入。

远程 MCP/plugin 中需要的凭据与模型不同，后续用独立 `ai_integration_secrets` 加密表；**不能把模型 API Key 移到那里**。插件请求模型调用时只传 profile ID，通过宿主调用，拿不到密钥。

**就近参考：**[Pi扩展契约](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/extensions/types.ts)。实际落点按本章/§7，不照搬源项目私有状态。

## 25. Hook 调用顺序、冲突与失败策略

事件建议：TaskStart、ContextCollect、BeforeCompaction/AfterCompaction、BeforeModelSelect、BeforeModelRequest、AfterModelResponse、BeforeToolCall、AfterToolCall、BeforeArtifact、AfterArtifact、TaskEnd/Error/Cancel。

Observer 只读；Interceptor 返回类型化建议/补丁/拒绝/要求审批，不能任意改整个 runtime state。关键调用链：

```text
完整参数解析→schema/业务初校验→before-tool Hook
→应用合法补丁→再次 schema/目标/风险校验→强制内置安全审计Hook并持久化→内核授权/审批
→再次校验资源 revision 与审批 hash→执行→保存真实回执
→after-tool Hook 生成显示/模型投影→向模型与 UI 发布
```

绑定以 priority 升序，再按 binding ID 稳定排序。Deny 优先于 RequireApproval，RequireApproval 优先于 Allow；插件 Allow 不覆盖内核拒绝。不同字段合法 patch 顺序应用，同字段 patch 显式冲突则暂停，不静默最后写入胜出。

错误策略按能力：审计/格式化等 best_effort 可以跳过；用户明确依赖的业务产物插件失败则该产物失败；安全性从内核保证，强制注册内置SecurityAuditHook；可选插件不能卸载/跳过该Hook。最终参数、授权变更需重新审计；审计无法执行/保存fail closed。Jev 优化强制 best_effort。Hook 有 deadline、payload/调用次数上限；递归调用 hook 禁止，宿主模型调用经过预算/隐私检查。

**实现补记（2026-10-07）**：第一批落 `BeforeToolCall`/`AfterToolCall` 两个事件，落点 `crates/fluxdb-app/src/parts/ai_hooks.rs` 的 `HookPipeline`/`AgentHook`。绑定按 `priority` 升序、再按 binding ID 稳定排序；同字段不同 patch 视为冲突并 fail closed；`Deny` 优先于 `RequireApproval`；after-tool 只接受 `Observe`，任何改写真实回执的返回都报错。补丁必须重新过 schema/风险/审批校验，且**不得改变执行目标**（改了直接失败）；最终参数回写 assistant 的 tool_call 以保证无状态协议重放配对。禁用绑定、取消、递归与超过 64 个绑定都有限制。第一批内置 Hook 是只读的 `fluxdb.lifecycle.telemetry`，由插件实例/绑定表装配；安全审计仍由内核强制、插件不可禁用。

AfterToolCall 不可将失败改成成功，不改写 execution facts；显示内容、模型投影的派生版本必须标记，真实 receipt 保留。

<a id="security-audit-hook"></a>

### 25.1 必经安全审计Hook与持久化判断

不是让 Jev/另一个模型判断“安全”再放行。主链：

```text
完整参数/Schema初校验
→可选 before-tool 插件补丁
→最终参数/目标/风险重新校验
→强制 SecurityAuditHook（针对最终plan）
→保存 prepared request + audit（同事务）
→拒绝 / 等待批准 / 允许
→执行前再核对授权revision、plan hash、取消
→必要时再次审计
→执行并持久化不可变回执
```

AuditInput：caller/owner、ToolId/version、目标/连接/grant版本、规范化参数hash、已解析的读写语义、资源/行数/时间限额、权限/规则集版本。不要把 API Key、加密载荷或全部数据样本传给审计。

AuditDecision：Allow / Deny / RequireApproval、risk、rule_ids、reason_codes、ruleset_version、plan_hash。内核最严格结果优先；模型审查插件最多增加警告/收紧，不能覆盖 Deny。审计无法执行/无法落盘时 fail closed，不能绕过后继续写。

独立表 `ai_tool_audits`（本文§42.11）保存 append-only 判断；InternalCall/McpRequest owner 二选一。工具参数修改后旧 audit/approval 不适用；过期重新审计。工具失败/denied/unknown 在UI能看到原因但不泄漏秘密。

**参考与落点：**[Goose ToolInspector/manager](/Users/shining3d/fusuwei/code/goose/crates/goose/src/tool_inspection.rs)、[permission inspector](/Users/shining3d/fusuwei/code/goose/crates/goose/src/permission/permission_inspector.rs)、[Pi beforeToolCall](/Users/shining3d/fusuwei/code/pi/packages/agent/src/agent-loop.ts)、[Claude PreToolUse](https://code.claude.com/docs/en/hooks)；落 app policy/permission、plugins/hook、tools/service、storage audits。借鉴控制点，不执行第三方shell审计脚本。

**就近参考：**[Pi before/afterToolCall](/Users/shining3d/fusuwei/code/pi/packages/agent/src/agent-loop.ts)；[Goose检查链](/Users/shining3d/fusuwei/code/goose/crates/goose/src/tool_inspection.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 26. 第三方插件：本期仅保留口子

<a id="extension-reserved"></a>

本期实现：原生 Agent、内置插件、自动可选 Jev、模型/工具/MCP 设置菜单、持久会话与时间线、必经安全审计 Hook、使用用户已经配置的 Docker MySQL/PostgreSQL/Redis 测试。

本期不实现：第三方插件安装/市场/热更新、Wasm runtime、外部代码执行、旧 AI 数据迁移、独立配置导入/完整应用备份产品、自动数据密钥轮换。保留能力接口、host_api_version、package/instance/binding 身份、typed manifest/schema；不为未交付功能加入 Extism/Wasmtime/JIT 依赖或显示可点击的安装按钮。

以后开放第三方插件时仍通过同一注册/权限/工具执行接口，不直接拿 Connection/SecretCrypto/GPUI。当前注册来源只允许 BuiltinRegistry；外部 runtime 返回明确 Unsupported，不默默运行未知文件。

**参考与落点：**[Pi 扩展契约](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/extensions/types.ts)、[runner](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/extensions/runner.ts)；落 app ai/plugins registry 和本文§23–26。借鉴可扩展能力，不照搬 JavaScript 加载器。

**就近参考：**[本期内置/预留边界](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-design.md#extension-reserved)。实际落点按本章/§7，不照搬源项目私有状态。

## 27. BI 与产物协议

`ArtifactSpec` 至少有类型、标题、schema version、来源 ResultRef/ToolCallId、指标口径、时间范围/时区、过滤条件、刷新语义、模板版本、可访问性描述。图表引用已存在结果字段，不让模型随意填写未查询的数字。

报表插件可主动提供 create/add_chart/add_table/finalize/export 工具，也可对最终结构化分析进行排版；不是只能在最后一段文字上挂 formatter。Agent 自主选择分析路径，插件控制产物规则与渲染说明。

默认产物：表格、折线/柱图、Markdown 报告；HTML 导出做转义/净化/禁止主动脚本和远程资源；PDF/XLSX 导出按依赖能力分阶段增加。没有 renderer 时返回可见不支持，不伪造下载链接。

报告完成需校验来源仍可用、字段/单位一致、截断/抽样明显、结论有证据。插件错误不改写真实 SQL 结果；缺业务口径时请求确认或明确标注假设。

### 27.1 产物声明示例

```json
{
  "schema_version": 1,
  "kind": "report",
  "title": "销售额分析",
  "renderer": "fluxdb.bi.report",
  "metric": {"name": "net_revenue", "definition_ref": "knowledge-ref", "timezone": "UTC"},
  "sources": [{"result_ref": "result-handle", "tool_call_ref": "call-handle"}],
  "blocks": [
    {"type": "chart", "chart": "bar", "result_ref": "result-handle", "x": "channel", "y": "revenue"},
    {"type": "table", "result_ref": "result-handle", "columns": ["channel", "revenue"]},
    {"type": "note", "text": "说明统计范围和数据限制，不填入未查询数字"}
  ]
}
```

示例句柄由宿主实际分配，不是可凭空创建的 ID；build 前校验字段存在/数值类型/来源作用域。内容/块数/标题/图表点数有上限，超限分页或聚合；产物中文字做编码/转义，不执行模型或插件生成的脚本。静态报告默认使用生成时快照；“刷新看板”需要显式重新执行权限和预算，不能在打开报告时偷偷查生产库。

### 27.2 报表是会话产物，不是设置页

BI/报表不放“大模型/工具与MCP”的设置菜单，也不作为Agent配置的普通表单页。Agent完成分析后，在会话中显示产物卡片：标题/renderer、关键指标、图表、统计口径、ResultRef+版本、ToolCallRef、完整/截断标记、快照与时区。

卡片提供SQL、导出、打开报告。**打开报告使用独立大弹框**，默认宽min(960dp,主窗口可用宽90%)、高不超过客户区85%，内部滚动；它是产物查看，不是“询问AI”的第三种展现形式，也不受询问AI的60%/40%宽度限制。

报告弹框仅读取已生成快照，不重新查询/刷新/调用模型。关闭/Esc/遮罩返回原会话、原抽屉/内嵌模式与草稿/滚动；内部点击不穿透，键盘焦点先在弹框。SQL作为弹框内可折叠来源区域，不能因查看SQL丢失报告。真实刷新是单独的明确动作，重新执行权限/预算与来源版本，不偷偷替换旧图。

会话产物或历史会话可以打开已有报告；本期不新增“BI设置”一级菜单。插件配置页只配置renderer/模板/格式等规则，不存放报告正文。

**就近参考：**[Goose工具输出展示边界](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ToolCallWithResponse.tsx)。实际落点按本章/§7，不照搬源项目私有状态。

## 28. 对外 MCP Server 与未来系统扩展

### 28.1 MCP Server：用户要求的主方向

fluxDB 原生工具通过 **本应用提供的 stdio MCP Server** 对外开放，具体实现见§28.4–28.8，13个工具见§20.5，Claude CLI测试见§37.2；[实现入口](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-design.md#mcp-server-implementation)。新增 apps/fluxdb-mcp 小型 headless binary，依赖 app/core/storage 和官方 Rust SDK，不依赖 GPUI、不启动内部主模型、不要求 fluxDB 的模型 API Key/Jev/Agent prompt。

Caller 区分 InternalTurn 与 McpSession，两者共用 ToolExecutionService、风险/授权、Connector 和 receipt；审计 owner 不混用，不创建假 Turn。客户端配置只含绝对命令/root/profile ID；秘密从已有 AES 连接凭据临时读取，不出现在 MCP schema、env/CLI、日志、返回和普通配置中。

Server Profile 默认 disabled/readonly，授权资源默认空；用户明确选连接/库或 Redis DB/对象范围后开放。一个 Server 分工具而不是一个无边界 execute；无桌面时只支持明确只读/范围预授权。Claude 客户端允许调用工具不等于 fluxDB 写批准，不提供模型自己 approve 的工具。默认 stdout 只协议、stderr 日志。

新增 ai_mcp_server_profiles/resource_grants/sessions/requests 独立表，详见 §42；与 ai_mcp_settings 区分 Server/Client。result/approval 的 owner 增加 MCP 分支，不把外部请求硬塞进内部 agent_sessions。取消、断开、未知写入与内部保持同样事实约束。

### 28.2 MCP Client：可选接入外部工具

原 ai_mcp_settings/ai_agent_mcp_bindings 对应本应用作为 Client 的配置。外部工具进入受筛选 registry，namespaced ID/schema/长度检查，不信任 readOnlyHint，不依赖外部 Agent daemon。stdio 环境中的秘密经宿主短期注入，HTTP redirect/URL/身份按权限限制。

Client 扩展可后做，不影响 Server 交付，也不影响原生工具；MCP 不是智能引擎。

### 28.3 MongoDB、HTTP、SSH

MongoDB 使用同样 target/tool/outcome 协议，不把任意脚本默认为读取；聚合 `$out`/`$merge` 区分风险。HTTP method 不足以证明无副作用，需域名/IP/redirect/SSRF 与用户授权。SSH 使用明确 host/user/auth ref、主机密钥验证、输出上限；不自动接受未知/变化 host key。这些能力分阶段增加，并经同一 Server Profile 决定是否对外暴露，不能默认公开所有内部工具。

<a id="mcp-shared-execution"></a>

### 28.4 内部Agent与外部MCP的统一调用链

```text
fluxDB 内部 Agent                       Claude Code / 其他 MCP Client
        │                                         │
   InternalCaller                          stdio JSON-RPC
        │                                         │
        │                                fluxdb-mcp 协议适配
        │                                         │
        └──────── ToolExecutionService ──── McpCaller
                       │
             解析/schema/资源/风险/授权
                       │
              审批或明确预授权范围
                       │
            Connector + bounded result store
                       │
             不可变回执/审计/进度/取消
                │                   │
              UI 投影          MCP 结构化投影
```

协议层不拼 SQL、不读密码、不绕过 app 直接调用驱动。所有调用只执行一次，使用统一 policy 和 receipt；外部 Agent 不启动 fluxDB 内部主模型，也不依赖 fluxDB 的模型 API Key/Jev/Agent prompt 配置。

**审计归属不能伪造内部 Turn：**InternalCaller 使用原有 session/turn/tool_calls；McpCaller 使用独立 server session/request 表。执行服务接受 `ExecutionOwner`，选择相应 repository，不创建一个假用户任务来满足 FK。结果存储支持两类 owner（本文§42.11）。

MCP 的 clientInfo/name 仅用于审计，不是认证/授权凭据。同 OS 用户启动的 stdio 进程具备本地访问能力；授权 Profile 防止模型误用，不宣称能防同账号恶意程序读取应用文件。

<a id="mcp-server-implementation"></a>

### 28.5 Headless Server代码、进程与Rust接入

#### 28.5.1 建议新增代码

- [apps/fluxdb-mcp/Cargo.toml](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-mcp/Cargo.toml)：新的小型 MCP 二进制；依赖 app/core/storage、选定 rmcp、tokio、serde、schemars，不依赖 GPUI。
- [apps/fluxdb-mcp/src/main.rs](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-mcp/src/main.rs)：参数解析、storage/profile 初始化、日志 stderr、stdio serve；不承载业务。
- [apps/fluxdb-mcp/src/parts/server.rs](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-mcp/src/parts/server.rs)：initialize/list/call/cancel、协议结果映射。
- [apps/fluxdb-mcp/src/parts/tools.rs](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-mcp/src/parts/tools.rs)：typed params 和 MCP 名称到 stable ToolId 映射。
- [app tools service.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/ai/tools/service.rs)：共用工具执行门面、caller/owner 和授权。
- [app mcp_server/mod.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/ai/mcp_server/mod.rs)：profile、grant、资源句柄、session/request、请求状态/限额。
- [storage mcp_server.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/ai_platform/mcp_server.rs)：独立表/CAS/审计。

所有都是拟新增。workspace 添加 member；打包三端 `fluxdb-mcp` / `fluxdb-mcp.exe`。真实安装路径由平台目录/打包结果解析，UI 生成客户端配置时输出绝对路径，不硬编码开发机目录。

#### 28.5.2 启动参数

```text
fluxdb-mcp --transport stdio --server-profile <id> [--config-root <approved-root>]
```

默认 root 复用 FileStorage 平台目录；测试可指定隔离 root。参数没有 DB password、API Key、任意 SQL 或未授权 connection URI。Profile 不存在/禁用/无授权返回明确错误或空能力列表，不能默认开放所有已有连接。

stdio 首版不绑定 TCP，不需端口/HTTP token。桌面未启动时服务也能运行；UI 设置 Server Profile 和资源 grants，导出不含秘密的客户端 JSON。外部 Agent 启动 MCP 子进程不等于 fluxDB 使用外部 Agent 执行自身任务。

#### 28.5.3 Rust 接入草图

以下是参考本地 rmcp 用法的边界草图，具体类型/feature 必须在选定版本编译测试；不直接复制 workspace 的 rmcp `3.4.1` 版本为未经核对的新依赖。

```rust
#[derive(Clone)]
struct FluxDbMcpServer {
    service: std::sync::Arc<ToolExecutionService>,
    caller: McpCaller,
    tool_router: ToolRouter<Self>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryParams {
    target: String,             // 来自已授权资源列表，不是连接字符串
    sql: String,
    #[serde(default)]
    parameters: Vec<WireSqlParameter>,
    max_rows: Option<u32>,
    timeout_ms: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "lowercase", deny_unknown_fields)]
enum WireSqlParameter {
    Null, Bool(bool), Text(String), I64(String), U64(String),
    F64(f64), Decimal(String), Bytes(String), // Bytes 使用有界base64文本
}

#[tool_router(router = tool_router)]
impl FluxDbMcpServer {
    #[tool(name = "fluxdb_query", description = "Run one authorized read query. Writes are rejected.")]
    async fn query(&self, params: Parameters<QueryParams>)
        -> Result<CallToolResult, ErrorData>
    {
        let args = validate_query_params(params.0)?;
        let receipt = self.service.invoke(
            ExecutionOwner::Mcp(self.caller.clone()),
            ToolInvocation::query(args),
        ).await;
        // 普通执行/权限失败映射成 isError=true 的 tool result。
        // 只将协议/解析错误映射为 JSON-RPC ErrorData。
        map_receipt_to_mcp(receipt)
    }
}

async fn serve_stdio(server: FluxDbMcpServer) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    server.serve(rmcp::transport::stdio()).await?.waiting().await?;
    Ok(())
}
```

SDK生成schema必须与内部registry的input/output schema一致（测试比较），语法与类型校验在注册和执行两层落实。SDK stdio默认frame大小不能假定有界，必须验证其限制或接入bounded transport wrapper后才能生产使用。typed handler 不代替授权：new 构建时按 Profile 过滤 Router；list_tools 动态投影能力；call_tools 即使名称已隐藏也再次检查权限。版本/资源变化使旧句柄失效，不能依赖 client 缓存的 tools/list。rmcp cancellation/progress 接到实际 CancellationToken/on_summary，不只是停止发送文字。

<a id="mcp-server-grants"></a>

### 28.6 Server管理表、资源授权与空规则语义

本文§42 是 DDL 唯一基准，不在本节复制CREATE SQL。新增四个独立表：

| 表 | 内容 | 关键约束 |
|---|---|---|
| ai_mcp_server_profiles | 本应用对外服务配置、启用/只读/工具集/限额 | 对象级 revision/CAS，默认 disabled、write_mode disabled |
| ai_mcp_server_resource_grants | Profile 可访问的连接、库/namespace、对象/key范围、读写规则 | 独立 revision；connection_id 建列但不向当前全量重写 connections 加 FK |
| ai_mcp_server_sessions | 一个 stdio Client 连接的协议版本/审计身份/状态 | profile FK；clientInfo 不授权 |
| ai_mcp_server_requests | 每次调用的 server ID、tool、scope、hash、状态/回执 | session/profile composite FK；协议 request ID 只是关联，不当幂等键 |

`ai_mcp_settings/ai_agent_mcp_bindings` 保留为 **本应用作为 Client** 的外部 server 配置，不能与这四张对外 Server 表混用。

通用 ai_results 增加 MCP owner/source 引用并以 CHECK 保证内部 Turn 或外部 MCP session 二选一；ai_approvals 增加 MCP request owner，防建假 Turn。主设计补充 owner 归属/外键测试。

#### 28.6.1 授权 JSON 的空值语义

- Profile.allowed_tool_ids=[]：不开放任何工具，不解释为“全部工具”。
- object_policy={}：不开放对象；创建授权时必须明确选整库/限定表或整DB Key/限定prefix，不能把空规则当全库通行。
- SQL `all_objects=true` 才表示授权数据库内所有允许的读对象；否则 table_allowlist 非空。Redis `all_keys=true` 才表示当前DB全部Key；否则 key_prefixes 非空。这些字段按资源kind校验，不能借另一个数据库类型规则放行。
- write_policy={}、allow_write=0 或 Profile.write_mode=disabled 任一成立均禁止写；intent文字不是权限。
- connection_id/database/namespace 是列，table/key类型专属范围进入经过验证的对象JSON。旧连接被删除/同ID配置变化时授权失效，不自动迁到新连接。

受限写示例（仅一次性测试库；实际库还要server能力/事务校验）：

```json
{
  "allowed_operations":["update"],
  "allowed_objects":["orders"],
  "require_where":true,
  "predicate_policy":{"column":"id","operator":"=","allowed_values":["1"]},
  "max_affected_rows":1,
  "allow_ddl":false
}
```

这不是仅检查JSON就能保证正确：SQL AST定位所有对象/条件、绑定值匹配，事务中验证并在提交前核实；无法证明范围就不自动执行。Redis则按命令所有key位置验证prefix与操作白名单，不宣称普通写可以回滚。

<a id="mcp-write-authorization"></a>

### 28.7 写入授权与没有GUI时的处理

stdio 默认只读，主窗口不开也能查询；不开窗口时不能展示一个不存在的审批按钮。

Profile write_mode：
- disabled：写工具不列出，直接调用也拒绝。
- scoped：用户在管理页对测试/业务资源明确预授权受限操作；执行时依然检查匹配范围/真实数据库权限/预算，不是“所有写都允许”。
- desktop_approval：只有实际实现经过身份校验的桌面审批桥时可用；否则配置/启动明确 Unsupported，不假装已有能力。

Claude Code 自身的工具 permission、`--allowedTools`、prompt“我批准了”，都不等于 fluxDB 对 SQL/Redis 修改的授权。绝不公开 `approval.approve` 让模型批准自己。

本地审批桥若实施，Windows named pipe / Unix domain socket 经平台抽象、受限 ACL/认证和 profile/session 绑定；桌面通过 AppCommand 验证完整 plan hash，协议进程只收到决定，不接收 Key。Client 断开时取消未执行审批，已执行状态待核实；没有用户仍在线时不能悄悄批准后执行。

<a id="mcp-server-lifecycle"></a>

### 28.8 协议生命周期、错误、日志与限额

- initialize→initialized→tools/list/call；SDK 做版本协商，只宣告已经实现的 tools/cancellation/progress 能力。
- stdout 仅 JSON-RPC，日志/诊断 stderr；不能 println 启动欢迎语污染协议。所有响应 schema/结构都能被实际 CLI 消费。
- 配置错误/unknown tool/schema 错误按 SDK 协议错误处理；数据库错误/权限/timeout/oversize 为 tool isError，结构化 code 与不可重试信息。
- 限 frame bytes、并发请求、持续时间、输出/结果缓存、未完成请求数。客户端断开先 cancel，再回收句柄/池/子任务；未知写保留审计，不自动执行重试。
- progress 必须响应 client 提供的 token，不能假定所有客户端有进度 UI；业务完成以最终 receipt 为准。
- 避免长 SQLite 事务跨网络执行；同一个 Profile 多进程/客户端的授权 revision 实时复核。协议 request ID 可能被复用，不作为永远唯一的数据库 PK。
- Server Profile 变化后使旧 target/授权失效，SDK/client 支持时通知 tools/list_changed，否则 call 时仍强校验。

**就近参考：**[Goose Rust stdio入口](/Users/shining3d/fusuwei/code/goose/crates/goose-mcp/src/mcp_server_runner.rs)；[typed Server示例](/Users/shining3d/fusuwei/code/goose/crates/goose-mcp/src/tutorial/mod.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 29. 权限与隐私规则

权限交集：用户授予 ∩ Agent 配置 ∩ 插件声明与授权 ∩ 当前 Turn scope ∩ 连接/数据库权限；插件只能收窄，不能扩大。

审批 hash 包括规范化工具 ID/version、完整执行参数的 hash、目标/连接配置 revision、plugin digest、数据/计划版本和允许范围。SQL 文本或目标变化后审批失效。一次批准不是永久批准所有 SQL；用户可授予明确的时限/作用域 read scope。

日志只记录 ID、动作、阶段、耗时、数量和脱敏 code；不记录 HTTP 原始错误 body/headers、完整 prompt、密码列或 API Key。敏感表/列先最小化再出站；远程模型隐私范围单独授权。可选本地模型也不能绕过文件权限。

不声称能自动识别所有业务敏感信息。默认不把结果全量持久化，用户可选择保留；API Key、已知凭据和密钥文件始终禁止进入消息/结果/截图。

**就近参考：**[Goose审批检查](/Users/shining3d/fusuwei/code/goose/crates/goose/src/permission/permission_inspector.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 30. 预算、费用和并发

金额用整数 micro-USD，价格带版本/时间和来源。模型未知价格不当成免费；严格费用预算下，必须配置价格或禁止选用。token 估计、缓存计价、工具费用分别处理。

调用前 SQLite 短事务预留最大允许输入/输出估算费用；同一 request ID 不重复预留。所有 Agent、Jev、压缩和产物生成共用 ledger，按 policy/session/day 计算。不得持事务等待模型。

结束后按 actual usage 结算，缺失 usage 记录 estimated/unknown，保守保留预留；超时不因“没拿到结果”就退回所有费用。实际费用可能超过估计，硬预算只能约束下一次调用和配置的最大生成量，不能承诺供应商计费绝不超支。

限制：活动任务数、模型请求并发、连接读取并发、文件 IO 并发、max iterations、wall time、tool output bytes；起始默认值写在应用设置并可调整，测试用更小限额。资源相同的写操作串行；独立读取允许有限并行，结果按模型源顺序回填以维持工具配对。

### 30.1 费用透明与内部执行保护（无用户费用上限）

本期不提供“费用上限/每日预算/严格费用预算”等用户配置开关，Agent 不需要用户先研究价格才能用。仍保留两类不可关闭的东西：

- **用量透明**：主模型/Jev/摘要/报告生成的 input/output token、估算/实际费用记入 `ai_usage_ledger`，会话底部显示本次累计（示例稿显示估算值）。价格缺失不当成免费，只影响展示准确性。
- **内部执行保护**：最大迭代、任务总时限、活动并发、工具输出字节、结果行数、文件读取字节。有效值取系统默认与 Agent 配置的更严者；这是防失控的运行时护栏，不是“预算策略”产品。

**实现补记（2026-10-07）**：账本落在 `crates/fluxdb-storage/src/parts/ai_platform/usage_ledger.rs`。`reserve` 在短事务内先取写锁（避免并发消费同一余额），对 `(request_id, attempt)` 幂等，校验 turn/session 归属与终态（含 `cancel_requested`），按模型价格快照估算并写入 held 预留 + unknown 账本行；只有策略显式配置金额上限时才要求价格已知。`settle` 只允许 held→settled/unknown，重复回调幂等，缺 usage 保留预留并标记 unknown，价格快照决定结算值（缓存价缺失且 cache>0 → cost 记 null）。应用侧 `MeteredProvider` 统一包裹主模型与摘要调用（purpose=`main`/`compaction`），请求前预留、拿到 `Usage` 后结算。面板底部只显示用量与费用，**不提供用户费用上限开关**。

预留：将来若做企业级配额/团队限额，可在 `ai_budget_policies` 上新增可空的全局/组织级硬限额与超限行为，首版不暴露 UI、不阻塞使用。

**就近参考：**[Goose usage_ledger分表](/Users/shining3d/fusuwei/code/goose/crates/goose/src/session/session_manager.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 31. 设置菜单和美观的原生Agent界面

<a id="ai-settings-menus"></a>

### 31.1 设置菜单、子页与现有导航接入

在现有设置 Sidebar 新增 **大模型**、**工具与 MCP**，不要只在聊天面板塞小弹窗。

```text
设置 Sidebar（现有菜单保留）
  大模型
    模型列表 / 新建编辑 / 测试连接 / 获取模型列表
    连接状态（成功/失败/未连接）落库常驻；测试=真实最小对话请求（提示词「你好」）
    编辑态 API Key 预填为黑点，点眼睛用 AppController::reveal_ai_model_secret 短时解密
    默认主模型 / 默认决策模型 / 凭据状态
    每模型能力、价格和隐私许可
    （模型 ID 排在 API Key 之后：拉取依赖 Key，Key 仅作请求头、不落库/不写日志）
  Agent 配置
    左列表 + 基本 / 模型与提示词 / 工具与内置能力（提示词直接编辑，无模板/多版本）
  会话历史
    普通打开恢复上次会话，新建才开新草稿（运行记录在 M2 写入）
  工具与 MCP
    工具       每Agent启停、读写风险、作用域、超时/结果上限
    本地 MCP   Server Profile、资源授权、导出客户端配置、活动
    内置插件   开关、自动状态、绑定角色、允许的参数
    安全审计   规则/记录/批准状态（必经检查不能关闭）
```

未来的 MCP Client 可在工具与MCP内增加单独tab，当前不冒充已实现；第三方插件栏不显示安装能力。Agent/会话的入口在主窗口AI面板；提示词在Agent编辑中直接输入，费用/执行保护收进Agent管理的高级弹框，避免把设置Sidebar扩成十几项。

真实代码改动：`SettingsPanelSection` 新增 AiModels/AiToolsMcp；settings_panel_sections 从固定8项更新；render/header/title 一并穷尽匹配。AI 保存通过专用命令/repository，不调用普通 Settings TOML save-all。表单Input/Button/Select/Tab/Dialog使用gpui-component 0.6.0。

**参考与落点：**[当前枚举](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/app_state.rs)、[当前Sidebar](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/settings/navigation.rs)、[Goose SettingsView](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/settings/SettingsView.tsx)、[ModelsSection](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/settings/models/ModelsSection.tsx)、[ProviderConfigurationModal](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/settings/providers/modal/ProviderConfigurationModal.tsx)。借鉴清楚的分区和状态，不复制其React布局/配色。

<a id="ai-native-timeline"></a>

### 31.2 原生时间线布局与视觉交互规格

“询问AI”仅有抽屉/内嵌两种形式：首次默认抽屉，以后每次打开恢复上次形式，面板内可切换。抽屉覆盖右侧且最大60%主窗口宽，内嵌占最右侧布局且最大40%；正常最小宽320dp、默认宽分别460/420dp，窄窗口规则见§31.4。不再使用固定720px上限或自动切第三种完整工作区。设置页复用现有176px Sidebar。

```text
┌ 会话标题       新建会话   切换形式   历史 × ┐
│ 连接/数据库 chip · 只读/可写范围 · 任务状态 │
│                                           │
│ 用户目标与 @对象/附件 chips                │
│                                           │
│ ▸ 思考摘要  进行中 / 已完成 · 耗时          │
│ ▾ 正在分析 · 3个步骤                       │
│   ✓ 查找订单表                            │
│   ✓ 读取结构                              │
│   ● 执行查询                              │
│ ▸ db.query  测试库 · 120行 · 0.4s          │
│   安全审计：允许 / 等待批准 / 拒绝         │
│                                           │
│ 分析回答                                  │
│ [图表/表格/报告预览]                       │
│ 来源 · SQL · 口径 · 导出                   │
├───────────────────────────────────────────┤
│ @ / 附件 / 多行输入                       │
│ 模型:自动  Jev:可用/未配置  发送/停止       │
└───────────────────────────────────────────┘
```

本图仅结构草图，不使用文字符号代替实际AppIcon。具体规格：
- 8px间距网格；询问AI正文11.5–12px、次要标签10–10.5px、SQL等宽10.5–11px、行高≥1.45。
- 背景/边框/强调/危险色统一UiColors，避免大面积鲜艳颜色；状态同时有图标+文字，不只红绿圆点。
- 代码/参数/输出默认折叠；工具成功显示一行摘要，错误/审批优先展开；思考内容默认折叠，用户手动展开优先于流式自动状态。
- 不为每个token新建卡片；30–60ms合并文本delta，卡片身份稳定，不闪烁、不抢滚动。用户向上阅读时暂停自动滚动，显示新内容提示。
- 工具卡提供脱敏参数、SQL、目标、审计、耗时、状态、结果引用；大输出内部受限滚动/分页，不能把整个窗口撑成长图。
- 窄面板无横向大表溢出；打开结果可跳到现有数据区；报告打开独立弹框。报告与对话之间由来源引用关联，不重复复制大结果。
- 主任务loading、等待批准、正在取消、unknown写入有不同说明；禁止只显示spinner或把中断当成功。
- 历史会话列表按时间分组，支持重命名/归档/分页；首次打开/无模型/无历史/模型不可用有专门空状态。

**参考与落点：**[GooseMessage](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/GooseMessage.tsx)、[ThinkingContent](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ThinkingContent.tsx)、[ToolCallWithResponse](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ToolCallWithResponse.tsx)、[ToolCallArguments](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ToolCallArguments.tsx)、[ProgressiveMessageList](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ProgressiveMessageList.tsx)、[Pi assistant-message](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/modes/interactive/components/assistant-message.ts)。落 main_parts/ai/{timeline,reasoning,tool_card,session_list,settings}，不直接移植React或TUI组件。

<a id="public-reasoning"></a>

### 31.3 公开思考内容、摘要和执行计划边界

展示模型API**明确对用户公开**的思考内容/摘要，不宣称拿到所有模型私有内部思考链。Provider能力分别标注 public_reasoning/summary/none；none时只显示真实的“生成中”和公开计划，不编造“我正在思考...”文字。

- 公开 reasoning/summary 流映射成独立 message part，默认折叠，可持久化/恢复。
- Provider签名、加密reasoning blob、私有token/隐藏状态不作为可见正文；首版不落普通消息表。不支持重建该私有续接状态的协议明确标记，不因“持久化”保证恢复全部内部推理。
- 工具/计划由事件和公开模型输出展示；工具执行事实比思考中的猜测更可靠，不将reasoning中的数字视为查询证据。
- 模型切换后保持对话/目标/工具回执，不复制私有推理状态；必要时重建stateless请求，不重放已有副作用。

**参考与落点：**[Goose ThinkingContent](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ThinkingContent.tsx)、[Pi assistant-message](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/modes/interactive/components/assistant-message.ts)、[OpenAI reasoning summaries说明](https://developers.openai.com/api/docs/guides/reasoning)。落 Provider capability+公开ReasoningEvent投影；不是要求所有模型输出原始CoT。

<a id="ai-panel-presentation"></a>

用户确认的视觉方向：执行过程使用一张可折叠的分组时间线，顶栏说明步骤/耗时/调用与纠正数，内部节点保留路由、工具参数/回执、失败与修正；绿色表示完成、红色保留原失败，不能把纠正成功伪装为从未失败。已完成过程默认折叠，运行/需要处理时展开，用户手动状态优先。

报表卡片采用更舒展的白/主题适配表面、轻边框、指标小块、清晰折线/坐标、口径与来源说明；发送按钮与完成状态使用主题的绿色强调。参考截图中的示例数字不直接作为真实产品数据，UI稿以一致的模拟序列计算指标和环比。

### 31.4 抽屉/内嵌模式、宽度与记忆规则

用户明确要求：只提供两种形式，首次默认抽屉；每次打开使用上次形式，上次内嵌则下次仍内嵌，不能关闭后恢复成默认抽屉。切换入口必须在面板内，不能只能去设置页。

| 属性 | 抽屉 Drawer | 内嵌 Embedded |
|---|---|---|
| 布局 | 主窗口右侧覆盖层，不挤占数据库布局 | 主窗口内容区最右侧，参与布局并挤占可用区域 |
| 默认偏好宽度 | 460dp（建议初始值） | 420dp（建议初始值） |
| 正常最小宽度 | 320dp（建议初始值） | 320dp（建议初始值） |
| 硬最大宽度 | floor(当前主窗口可用宽度×60%) | floor(当前主窗口可用宽度×40%) |
| 调整方式 | 左边界拖拽，可拉宽/窄 | 与数据区之间的分隔柄，可拉宽/窄 |
| 关闭 | 关闭按钮、Esc、遮罩外点击，不穿透 | 关闭按钮；AI焦点内Esc在无子弹层时关闭，无遮罩 |

W是当前主窗口**客户区布局宽度**（逻辑dp，排除系统边框），不是整块物理显示器分辨率，也不是扣掉连接树后的编辑区宽。面板外盒含边框/内边距；阴影不计布局宽，手柄不能额外把外盒撑过上限。高DPI/跨屏时按当前窗口重新计算，不能保存物理截图像素。

拖拽/首次显示/窗口缩放/跨屏/模式切换都调用同一个纯函数：

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AiPanelMode { Drawer, Embedded }

fn resolve_panel_width(mode: AiPanelMode, desired_dp: f32, viewport_dp: f32) -> f32 {
    let ratio = match mode { AiPanelMode::Drawer => 0.60, AiPanelMode::Embedded => 0.40 };
    let max_dp = (viewport_dp.max(0.0) * ratio).floor();
    let min_dp = 320.0_f32.min(max_dp);
    desired_dp.clamp(min_dp, max_dp)
}
```

输入进入该函数前验证finite/nonnegative，宽为0时暂不渲染。现有WindowOptions没有指定全局最小窗口尺寸，因此不能假定W≥800dp。W太窄时固定320dp最小与比例最大不能同时成立：**有效最小=min(320dp,本模式硬最大)**，临时采用紧凑布局/换行，不突破60%/40%、不自动改模式、不新增第三种全屏形式。正常窗口下最小始终320dp。

例：W=1200dp时抽屉≤720、内嵌≤480；W=1600dp时分别≤960/640；W=700dp时内嵌≤280、有效最小280，模式仍内嵌。若用户以后要求绝对320最小，应另行约束应用最小窗口宽≥800，不在本次无声修改全局窗口限制。

两个模式分别记宽度；从460抽屉切内嵌不直接套用460，而用内嵌上次宽度并clamp。窗口变窄只调整实际展示宽，不覆盖用户偏好宽；用户主动拖拽结束才保存新偏好。模式切换/拖拽松开异步保存CAS，失败提示“本次已应用，重启后可能未保留”，不假装已持久化。

**参考与落点：**[gpui-component Sheet](/Users/shining3d/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-component-0.6.0/src/sheet.rs)、[ResizablePanel/size_range](/Users/shining3d/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-base-0.6.0/src/resizable/panel.rs)、[主窗口布局](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/navicat_main/render.rs)、[WindowOptions](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/app_boot.rs)。实现放main_parts/ai/panel与layout纯函数；不可用物理screen宽硬编码。

<a id="ai-panel-session-open"></a>

### 31.5 打开、新建、关闭和恢复哪一个会话

建议采用：**普通打开恢复上次实际查看的会话；“新建会话”才显式开始新的空白草稿。**这是fluxDB的产品决定，不把几个参考项目当成统计上的“大多数”。Goose导航实现优先currentSession/lastSession再打开；VS Code文档将新建、切换、关闭显示和删除分开，也恢复聊天/布局与草稿。借鉴这种分离，不复用其会话协议。

打开规则按当前应用配置root持久化，不按某条SQL标签页各复制一个面板：
1. 先加载preferred_mode与该模式宽度；打开不是切换模式或调用模型。
2. last_session_id有效且未归档：恢复该会话、草稿、展开/滚动锚点和当前执行状态。
3. 首次/没有指针/会话已归档或不可用：显示空白新会话草稿；不随机选“更新时间最新”的另一会话。
4. “新建会话”清空选择指针并打开新草稿，保留旧会话；首次发送才事务创建Session与首个Turn，反复打开/新建空草稿不生成大量空历史。
5. 已有Session的未发送内容先保存到其ui_state。尚无Session的新草稿如有内容，新建前询问是否放弃；取消则原样保留。关闭后重开恢复该新草稿，不能又跳回旧会话。
6. 历史会话点击后更新last_session_id；后台旧任务完成不更新这个指针，避免重新打开跳到后台任务。

面板标题栏固定提供 **新建会话、切换形式、关闭**；历史入口可独立按钮/菜单。切换形式菜单只有“抽屉/内嵌”，标记当前选择。抽屉用Sheet原有关闭按钮（不要再重复画一个X），内嵌自己用Button/AppIcon关闭。

关闭=隐藏UI，**不删除会话、不新建、不发送、不等同停止**。运行中的任务继续受原授权/预算/超时约束，主窗口入口显示运行/等待徽标；停止在输入区独立按钮。等待批准仍等待，关闭/遮罩/Esc不能算批准。重新打开只重接同一runtime事件订阅，不重新执行工具或自动Resume interrupted/unknown任务。

切换形式同样保留SessionId、草稿、@引用、公开思考、工具卡、滚动位置和任务；只有布局变化。普通恢复显示原会话上次目标；当前数据库页面已变化时明确显示“会话目标A/当前页面B”，不静默切换SQL/Redis执行目标，retarget仍走§13的显式动作/重新授权。

应用重启后默认AI面板不自动弹出（open是内存状态），但用户再次打开时恢复上次形式/会话或新草稿。关闭主应用的取消/恢复按§11，不与关闭AI面板混为一谈。

**参考与落点：**[Goose handleNavClick/currentSession/lastSession](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/hooks/useNavigationSessions.ts)、[VS Code会话/新建/恢复文档](https://code.visualstudio.com/docs/agents/run/sessions/manage-sessions)。落app命令/独立偏好repository、main_parts/ai/{panel,composer,session_list}。

### 31.6 布局偏好持久化与组件复用

新增独立表ai_panel_preferences（§42.10）：singleton保存preferred_mode、两种偏好宽度、last_session_id、无Session时pending_draft_json、revision；不塞kv，也不把它混进模型runtime设置。每Session草稿/展开/锚点仍在ai_session_ui_state。新建后last_session_id=NULL是显式新草稿状态，不用“最近历史”补回。

命令：OpenAiPanel、CloseAiPanel、SetAiPanelMode、SetAiPanelWidth、StartNewAiDraft、SelectAiSession；模式/宽度仅更新UI偏好，不调用StartAiTurn。第一条消息发送时create_session/start_turn与偏好指针、pending_draft清除同事务；失败不留下半个空会话或丢失草稿。

同一个AiPanel内容Entity只挂载一次，两种容器共享它；不可创建两个runtime/重复订阅或把布局隐藏触发Drop取消任务。模式选择/宽度保存为防抖CAS，关闭前发出flush；模式可见更新和存储成功状态明确区分。

优先复用Sheet和h_resizable/ResizablePanel。已经检查0.6.0源码：Sheet虽有resizable(bool)字段和setter，但本版本render没有消费该字段，也没有比例限制/resize结果回调；**不能只写.resizable(true)就宣布满足要求**。抽屉在现有Sheet/底层Sheet surface边界组合受限ResizablePanelGroup，内嵌用相同组件；用size_range与窗口变化后同一clamp约束。若高层Sheet无法承载组合，通过已安装底层Sheet的surface/close/focus能力复用，不重造普通按钮/输入。

内部菜单/Esc先关闭子弹层，之后才处理面板关闭；切形式时先拆掉旧容器遮罩/焦点约束，再接新容器，不让内嵌仍残留遮罩。实例复用与恢复焦点通过真实E2E验证，不能仅看尺寸函数测试。

API Key输入框默认掩码`***`，输入框内末尾提供眼睛按钮：点击后才解密显示真实Key（短时、仅前台、不写日志，关闭弹框/超时/失焦立即重新掩码）；默认Keep、明确Set/Clear；批准必须明确范围/hash，Esc/遮罩绝不视为批准。轻量反馈统一show_message。

**就近参考：**[当前设置Sidebar](/Users/shining3d/fusuwei/code/fluxDB/apps/fluxdb-desktop/src/main_parts/settings/navigation.rs)；[Goose思考/工具组合](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/GooseMessage.tsx)。实际落点按本章/§7，不照搬源项目私有状态。

### 31.7 整个平台的HTML交互稿

**实现对照入口（本文顶部也列出）**：[ai-agent-ui.html](/Users/shining3d/fusuwei/code/fluxDB/docs/prototype/ai-agent-ui.html)，样式/脚本按职责拆文件，复用仓库Lucide SVG，离线不依赖CDN。

覆盖整个AI Agent功能：数据库工作台、抽屉/内嵌会话、模式/宽度/会话记忆、新建/关闭、公开摘要与执行时间线、模型Keep/Set/Clear演示、Agent主页面编辑（列表+右侧五分区）、Agent内直接提示词、12个内置插件/Jev开关、13个工具、本地MCP授权/配置导出、安全审计、会话历史、报表弹框/来源/导出，以及高级费用/执行保护。

这是浏览器交互稿：所有数据/模型/工具/MCP执行均模拟，不实现AES、不读取实际附件内容、不连接Docker、不调用Claude或模型。表单不能输入真实密钥，密钥字节不保存localStorage。截图测试只证明HTML交互，不替代§37的真实GPUI/连接/MCP E2E。

主设置只有“大模型/工具与MCP”；报表由产物卡片/历史打开弹框，高级保护不是首次使用前置页面。浏览器localStorage仅模拟布局/演示状态，不是产品SQLite持久化实现。

### 31.8 Agent配置分区、直接提示词与字段存储

交互稿参考用户提供HTML的列表+配置主页面与分区结构（本期三个分区：基本、模型与提示词、工具与内置能力），但按本文实际能力实现（不使用弹框）。**没有Jev专用配置/决策模型下拉框，也没有提示词模板/版本选择器**；Jev仍由大模型配置与内置自动装配管理，不要求每个Agent设置。

| 分区 | 当前字段 | 对应存储 / 行为 |
|---|---|---|
| 基本 | 名称、说明、启用 | ai_agents通用列 |
| 模型与提示词 | 默认生成模型、system_prompt直接输入、压缩触发比例（默认80%） | default_model_profile_id列、system_prompt列、options_json经能力校验 |
| 工具与内置能力 | 可用工具、BI/文件/上下文/路由/压缩等内置能力 | allowed_tools_json及独立plugin binding；安全审计/内核护栏不能关闭 |

不再作为 Agent 用户配置：权限（只读/写入、文件根、数据出站）由连接只读标记、内核安全审计与§29权限交集决定，用户在“工具与MCP/连接”层管理；运行限制是§30.1的内部护栏；目标自动捕获（§13）；路由属插件；费用无上限。

正常默认值：40迭代、600秒、2活动任务、128KiB工具投影、200行、256KiB文件读取；均在真实实施时结合Connector能力验证。保存拒绝非finite/非正限额、行数>1000、不支持的生成参数和越权绑定；提示词长度按UTF-8 bytes检查，不能只看浏览器字符数。

Agent直接编辑提示词与其他字段一起SaveAiAgent(expected_revision)；可关联独立预算策略/插件绑定时同事务逐对象CAS，不save-all整个AI配置。Agent没有APIKey字段，模型秘密只在模型行。复制Agent可以复制提示词正文和合法引用，不引入提示词发布或历史管理。

修改运行中Agent只影响下一Turn。当前Turn快照包含agent_id/revision/system_prompt/hash、实际模型、权限和限额；编辑不会篡改历史消息/工具/审计。数据出站或读写策略变化需下一执行边界重新校验。

Agent配置是主页面而非弹框：左侧Agent列表（内置置顶），右侧标题/启用+三个分区，底部未修改/复制/保存栏。分区切换保留未保存输入；切换Agent时若未保存需确认放弃；保存失败保留输入并显示原因。文本域都是直接输入；在询问AI内只显示所用Agent/配置状态，不堆版本ID。

**参考与落点：**用户参考HTML的Agent主页面/分区（只借鉴结构/视觉，不直接替换文件）；[Agent状态/命令](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/state.rs)、本文§19/§23/§29–30/§42.10。落ai/agent_editor与Agent repository，UI不读写驱动/凭据。

<a id="approval-ui"></a>

### 31.9 审批交互：询问AI内联批准卡片

写操作/DDL/危险Redis/HTTP-写/SSH 等需要批准时，**直接在“询问AI”面板的对话流里内联一张批准卡片**，不另开弹窗、不跳设置。参考实现对比后采用“Goose三态按钮 + 数据库场景信息密度”的组合：

| 参考 | 机制 | fluxDB取舍 |
|---|---|---|
| [Goose ToolApprovalButtons](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ToolApprovalButtons.tsx) | Allow Once / Always Allow / Deny 三态按钮；已点/已允许/已拒绝/已取消/请求过期(stale)状态 | 采用 Approve Once / Deny 两态 + 状态回显；**不提供永久 Always Allow**（见下） |
| [Goose ToolCallConfirmation](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ToolCallConfirmation.tsx) | 行内确认块展示工具请求/响应与风险 | 采用行内卡片结构，替换为数据库上下文与SQL/命令 |
| Codex App Server / Claude Code | 服务端发出approval请求，UI恢复会话时仍需处理；拒绝/允许为显式事件 | 采用同一事件模型（AppEvent），刷新/重启后批准卡仍可处理，不丢状态 |
| Pi | 明确不提供内置权限系统，由宿主提供 | fluxDB内核强制审计，不把权限下放给插件 |

卡片结构（自上而下，全部使用UiColors/主题色，风险高时琥珀边框+徽标）：

1. **头部**：`需要你批准这次写入` + 风险徽标（高/中/低）；关闭/切走面板不等于批准。
2. **上下文行**：连接环境（Docker MySQL ·测试库，附“非生产”徽标）、库/schema（PG schema、Redis DB）、目标连接配置 rev（写明“配置已变更将使批准失效”）。
3. **计划内容**：SQL/Redis argv 的只读代码块，等宽字体，可复制；多语句逐条编号。
4. **影响预估**：`预计影响行数`（先 SELECT COUNT 核实，未核实写“未知”）+ `依据`（内核实测/事务内再校验/仅按语法估算）。
5. **约束清单**：可勾选项只读展示内核已判定的护栏命中情况（如“仅本条 UPDATE”“必须带 WHERE”“事务内复核命中行数≤N”）。勾选框**不可由用户取消**——它们是事实，不是开关。
6. **提示**：Esc / 点击遮罩 = 拒绝本次；写操作失败不自动重放。
7. **按钮**：`拒绝`（次要/危险色）+ `批准一次`（主色绿色）。可选勾选“在 10 分钟内对同一 plan hash 免重复审批”——这是**带过期与hash绑定**的范围授权，替代Goose的永久 Always Allow；超出范围、参数变化、连接rev变化立即失效。

状态机：卡片状态 `waiting → approved_once / approved_scoped / denied / expired`；按钮点击后原地变为结果回显（已允许/已拒绝），不闪烁、不重复可点；拒绝时给模型结构化原因，让下一轮换方案而不是重试同一写入。等待期间工具receipt落库为 waiting_approval；MCP外部客户端无GUI时返回 ApprovalRequired 错误码（§28.7），不假装已批准。

键盘与可达：批准/拒绝为原生按钮、Tab可达；Esc 拒绝（与遮罩一致）；焦点进入卡片时读屏播报“等待批准：<工具> <目标>”，批准后播报结果。绝不提供“全部允许/不再询问”的全局开关。

**就近参考：**[Goose ToolApprovalButtons](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ToolApprovalButtons.tsx)、[Goose ToolCallConfirmation](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ToolCallConfirmation.tsx)、[Codex App Server](https://developers.openai.com/codex/app-server/)、本文§11/§20/§25/§29。落 main_parts/ai/approval + AppEvent(AiApprovalRequested/Resolved)。

## 32. 初始化、设置加载与错误显示

启动只做必要 schema/init 和小索引，AI entity 列表分页，会话/消息按需加载。无模型时面板说明“请配置主模型”，不影响数据库工具；无 Jev 时不显示错误条。

聚合视图 `AiManagementView` 只用于读取，不能作为整体 save-all DTO。每个实体独立 Save(expected_revision)，冲突提示重载/比较；不静默覆盖另一个窗口或测试进程修改。

设置加载失败给明确诊断；损坏 JSON/未知 provider/不兼容插件使该实体 unavailable，不用默认空配置重写损坏对象。数据库写入失败时保留 UI 草稿，成功后才更新 revision。

**就近参考：**[Goose模型分区](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/settings/models/ModelsSection.tsx)；[当前FileStorage](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/lib.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 33. 凭据加密与当前密钥兼容

当前代码实际使用 config.toml 的 `connection_secret_key`，未配置时使用内置 DEFAULT_KEY；现有注释已说明内置密钥不能防同时得到程序和数据库的攻击者。规范目标为 `secret_key`。不得假定规范名字已在代码实现。

提取共享 `SecretCrypto`/KeyProvider：先读 `secret_key`，再读 legacy `connection_secret_key`；两者均存在且不同则拒绝操作并说明冲突。两者相同合法。连接和模型调用同一个 provider，保留连接旧 AAD/密文格式；普通 settings 保存必须保留密钥字段，不把它加入可导出 Settings DTO。

新安装且无已有密文时生成随机 32 字节密钥，使用安全、跨平台的原子文件创建/替换和权限收紧，成功保存后才能写密文。多进程初始化必须锁/原子创建，不能不同进程各写一把钥匙。存在旧凭据且无显式密钥时保持旧钥匙兼容并显示安全提示，不自动生成新钥匙导致旧数据失效。兼容内置密钥不宣称强抵御离线攻击；本期保留已有钥匙，不自动重加密或迁移连接凭据。

模型 AAD 使用无歧义编码的 `(domain="fluxdb-ai-model-key", cipher_version, profile_id, key_id, secret_revision)`；不含可编辑 display_name/provider/model，Keep 改配置不需重加密。每次 Set 生成新随机 12 字节 nonce；aes-gcm 返回 ciphertext 已含认证 tag，不另造冗余 tag 列。

密文/nonce/cipher_version/key_id/secret_revision 仅 private storage row。读取状态可以检查存在/密钥匹配并给 Configured/Unavailable；普通 API 不返回这些字段。transport 临时获取解密 lease，使用后尽力清零，明确内存清零不保证消除所有底层 HTTP/TLS 拷贝。

密钥错误/损坏/未知版本：拒绝解密和 Set/Clear 覆盖，要求修复或显式恢复流程；不得自动把 Unavailable 改成 Missing 后重写。显式恢复先备份且确认哪些旧密文不可恢复，不混在普通保存中。

### 33.1 共享 AES 原语草图

```rust
fn seal_model_key(
    cipher: &aes_gcm::Aes256Gcm,
    profile_id: &ModelProfileId,
    key_id: &KeyId,
    secret_revision: u64,
    input: &SecretInput,
) -> AiResult<EncryptedSecret> {
    use aes_gcm::aead::{Aead, Payload};
    use rand::RngCore;
    let mut nonce = [0_u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let aad = encode_model_aad(profile_id, key_id, secret_revision, 1);
    let ciphertext = cipher.encrypt(
        aes_gcm::Nonce::from_slice(&nonce),
        Payload { msg: input.expose_for_storage(), aad: &aad },
    ).map_err(|_| AiError::CredentialEncryptionFailed)?;
    Ok(EncryptedSecret { ciphertext, nonce, cipher_version: 1,
                         key_id: key_id.clone(), secret_revision })
}
```

只在 storage 的专用秘密路径提供 expose_for_storage；普通 DTO/插件不能调用。encode_model_aad 用版本化长度前缀编码，不能拼接无分隔字符串；读/写对称，回归测试防更改编码导致旧 Key 不可读。KeyProvider 先认证已有密文再允许 Set/Clear，私有密文结构不导出。

**就近参考：**[当前AES/Key/AAD](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/connection_secrets.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 34. 凭据保存、CAS 与故障原子性

保存 API：`save_model(edit, expected_revision, ApiKeyChange)`；普通 edit 不能携带加密列。

```text
校验 profile/edit/SecretInput
→ BEGIN IMMEDIATE → 读取 private row 与 revision
→ 检查 expected_revision
→ 仅 Set 或清除已有秘密的 Clear 获取共享密钥并认证旧密文
→ Keep: 不触碰任何秘密列
   Set: 加密并增加 secret_revision
   Clear: 清空秘密列并增加 secret_revision
→ UPDATE ... WHERE id=? AND revision=? → 确认 affected_rows=1
→ 同事务更新对象 revision/时间 → COMMIT → 返回非敏感 DTO
```

CAS 冲突返回 Conflict，整事务回滚；加密/SQLite commit/磁盘满失败保留旧配置与旧 Key。Set 不通过连接凭据表先写再补偿，也不使用全量 save-all。

### 34.1 保存现有模型的事务草图

```rust
fn save_existing_model(
    &self, edit: ValidatedModelEdit, expected: u64, change: ApiKeyChange,
) -> AiResult<ModelProfileView> {
    let mut conn = self.open_ai_connection()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let row = load_private_model(&tx, &edit.id)?;
    if row.revision != expected { return Err(AiError::Conflict); }
    match change {
        ApiKeyChange::Keep => update_public_columns_cas(&tx, &edit, expected)?,
        ApiKeyChange::Set(secret) => {
            // 错误 Key 不能通过 Set 绕过数据丢失防护。
            let crypto = self.crypto.verify_existing(&tx, &row)?;
            let encrypted = crypto.seal_model_key(&row.id, row.secret_revision + 1, &secret)?;
            update_model_and_secret_cas(&tx, &edit, expected, encrypted)?;
        }
        ApiKeyChange::Clear => {
            if row.has_secret() { self.crypto.verify_existing(&tx, &row)?; }
            clear_secret_and_update_model_cas(&tx, &edit, expected, row.secret_revision + 1)?;
        }
    }
    let view = load_public_model_view(&tx, &row.id)?;
    tx.commit()?; // commit 失败不得把 view 发给 UI。
    Ok(view)
}
```

update_* 的每次 UPDATE 都必须检查 affected_rows=1，成功将 revision 增一；Keep SQL 的 SET 不包含秘密列。创建模型另一个事务入口（expected 不存在），不能把 CAS=0 当成 blind upsert。verify_existing 仅在存在密文时认证；Keep 不解密/改写旧秘密，密钥不可用时仍可修改名称或禁用模型并保持 Unavailable 状态。没有秘密且不涉及 Set 的无认证模型不能因为缺密钥阻断编辑。整个函数不将 SecretInput/EncryptedSecret 写入 tracing。

未配置 auth=none 的模型为 NotRequired；api_key 且无密文为 Missing；有可用密文为 Configured；有密文但认证失败为 Unavailable。API Key 的最小非空/长度/控制字符限制在应用层，不能 trim 后改变用户密钥。

本期不实现全局数据密钥轮换；模型Set只是用同一共享钥匙替换该行APIKey。未来轮换需独立可恢复设计，不能通过手工改secret_key造成已有密文不可读。

**就近参考：**[当前凭据错误/失败注入](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/credential.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 35. 持久化总体规则

所有 AI 管理、会话、执行、usage、审批、产物元数据、知识均独立表。JSON 仅作为**一行对象内部**的 provider/plugin/schema/spec 参数，不在 kv 中塞整个平台。大文件是受控产物文件，独立表保存相对位置/大小/hash/状态；不是 kv。

AI 管理 DDL 见 §42.10，运行/辅助 DDL 见 §42.11。实现时唯一源码为 `ai_platform/schema.rs`；本设计作为评审基准，不再在 sqlite.rs 手写复制 DDL。所有打开 AI 表的 SQLite Connection 必须 foreign_keys=ON、busy_timeout；WAL 保持现有策略。

管理实体一行一个 revision 做 CAS。会话追加型记录使用 seq/unique 与事务，不全量重写历史。现有 connection_store 的普通保存会 DELETE 全表再插入，因此本阶段不向 connections 添加 AI FK：知识条目的 connection_id 单独建列，由应用校验/标记失效，避免破坏现有保存；不顺手改连接存储。模型/Agent 等被引用时逻辑 disable；显式删除按 FK 拒绝或有确认的完整依赖清理，不隐式级联删除历史。

默认不保存完整结果集；按用户保留策略保存消息必要内容、执行摘要/引用。保留数据带 retention/redaction 状态；同库权限控制不等于全数据库加密，报告包含业务数据时仍需用户管理导出位置。

<a id="session-message-parts"></a>

### 35.1 消息头、parts与执行事实的单一真相

参考对比：
- [Pi session-manager](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/session-manager.ts)：稳定ID、append-only entry、model/usage/compaction等有类型事件和parentId。采用稳定ID/checkpoint思想，首版线性会话，不直接使用JSONL分支树。
- [Goose session_manager](/Users/shining3d/fusuwei/code/goose/crates/goose/src/session/session_manager.rs)：独立sessions/messages/usage_ledger。采用分页、会话与消息/费用分离，不整体load-all/save-all。
- [OpenCode session.sql.ts](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/session.sql.ts)：SessionTable/MessageTable/PartTable分离。采用message/part身份与局部更新；fluxDB的tool/approval/usage仍用自己的事实表。

本期基线 DDL 在本文§42.10/§42.11，新增：

| 表/对象 | 职责 | 单一真相 |
|---|---|---|
| ai_messages | message身份、role、session/turn/seq、metadata | 不再重复保存整份正文content_json |
| ai_message_parts | 按ordinal的text/reasoning_summary/plan/context/tool/result/artifact/notice | 各part payload仅本部分正文；工具/结果/产物是引用列 |
| ai_session_ui_state | 选中/展开/滚动锚点/可选草稿、revision | 本地显示状态，与模型上下文分离 |
| ai_tool_calls / MCP requests | 参数hash、prepared/running/failed/unknown、receipt | UI卡片执行状态来自这里，不复制到part正文 |
| ai_tool_audits | 最终plan的审计判断与规则版本 | append-only，不改历史审计 |
| ai_context_checkpoints | 摘要+保护条目+source范围/hash | 原始消息不因压缩被删除 |
| ai_session_model_selections | 草稿/会话级模型覆盖（`scope_id='draft'` 或 `session_id`、`model_profile_id`、`revision`） | 显式选择优先于 Agent/全局默认；首次发送原子转移 |
| ai_context_references | 草稿/会话的显式 `@`/附件引用（opaque id、标签、来源、摘要） | 只传摘要，不把路径/正文/写权限交给模型 |

message/part组装到普通SessionView；模型边界另做投影，默认不把UI状态或审计卡片当用户指令。插件不能写任意消息role或真实receipt。

**参考与落点：**上述三个存储实现；落 storage ai_platform/{sessions,messages,parts,ui_state,audits}、app runtime/event、main_parts/ai/timeline。

<a id="session-streaming-restore"></a>

### 35.2 流式落盘、崩溃恢复、分页与UI状态

1. iteration开始事务创建assistant message头和text/reasoning part ID，UI keyed于稳定ID；没有每token一个message。
2. delta只在内存/事件流追加；每250–500ms或达到4KiB合并partial part checkpoint。deadline/字节阈值是可调起始值。
3. finish事务写最终payload、status、时间与message persistence_kind=final；tool开始/审计/审批/receipt立即持久化，不等正文flush。
4. 异常/退出尽力flush；强制崩溃最多损失最近一段非关键文字，工具副作用按原unknown规则核实。重启streaming part为interrupted，不能继续spinner。
5. 按message seq游标分页，初始加载最近50条，每页parts集合分批查询，不N+1也不一次整个历史。长parts渲染按大小和内容分段。
6. UI state和面板preferred mode/width独立CAS/debounce，折叠/滚动用part/message稳定ID，不用易漂移的数组下标。恢复先核对引用是否存在；失效状态跳过，不改正文事实。
7. 每轮snapshot绑定Agent revision/提示词快照、model/plugin/grant revision；新Jev绑定不改变旧历史显示。

持久化验收：关闭重开后回答、公开摘要、SQL、工具结果引用、审计、批准/拒绝、产物、费用和展开状态正确；停止与强杀区分，未知写绝不重放。UI画面来源是重建的数据库状态，不是缓存里的对话截图。

**参考与落点：**[Goose渐进消息加载](/Users/shining3d/fusuwei/code/goose/ui/desktop/src/components/ProgressiveMessageList.tsx)、[Pi append/checkpoint](/Users/shining3d/fusuwei/code/pi/packages/coding-agent/src/core/session-manager.ts)、[Goose会话存储](/Users/shining3d/fusuwei/code/goose/crates/goose/src/session/session_manager.rs)；落 parts repository、runtime coalescer、timeline list。

**就近参考：**[当前SQLite/WAL/建表](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/sqlite.rs)；[Goose独立session/messages](/Users/shining3d/fusuwei/code/goose/crates/goose/src/session/session_manager.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 36. 首次建表与数据生命周期（不做旧数据迁移）

当前没有旧AI schema/kv/session 数据，删除旧AI导入、ai_schema_migrations、migration checksum/升级引擎任务。首次打开在短事务幂等创建全部表/索引/trigger、seed内置能力；失败不覆盖已有连接/history/普通kv。发现同名表结构不符就明确错误，本期不自动转换未知表。

现有连接共享密钥的字段兼容保留，因为需要读已有连接凭据；这不等于迁移AI历史。普通Key替换只更新模型行，不轮换全局数据密钥。

保留必要的安全规则：普通导出不含API Key/密文、报表安全写入、历史/结果清理、测试一致快照。完整配置导入/备份UI、密钥轮换、未来schema升级仅列未来边界，不作为当前交付依赖。

**参考与落点：**[sqlite::create_schema](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/sqlite.rs)、[当前连接密钥读取](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/connection_secrets.rs)；本文§36改为“首次建表与数据生命周期”。

**就近参考：**[当前首次建表入口](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/sqlite.rs)。实际落点按本章/§7，不照搬源项目私有状态。

## 37. 测试架构与真实“智能”评测

三层必须分开报告：

1. 单元/存储/策略测试：稳定 fixture，验证 CAS、加密、schema、预算、工具风险、Hook、压缩边界。
2. 界面 E2E：scripted Provider 通过正常 UI→AppCommand→runtime→真实测试数据库/文件→回执→UI；保证界面到执行完整链路可复现，不只录制一段对话文字。
3. 真实模型任务评测：有明确授权、费用上限、固定模型/任务/工具；验证自主探索、错误修正、报告证据和复杂任务质量。不能把 scripted 的固定工具调用称为已证明智能。

策略/存储单测用隔离SQLite/mock；真实GUI/MCP测试复用用户已经配置并授权的Docker MySQL/PostgreSQL/Redis与AES连接凭据，创建本run数据库/schema/keyprefix。使用受控一致配置快照作为test root，不新增手填连接、不改用户AI设置；密钥配置不进证据/模型。后来新增的未确认连接不能推断为可破坏。测试主窗口的 storage root 注入必须是 debug/test-only、新增且明确标注，当前没有假定存在可用 AI debug 环境变量。

预期检查至少覆盖：未知表探索、方言差异、错误修正、结果截断、Redis 扫描、文件编码/越权、@ 同名/失效、标签切换不改执行库、写审批、未知写结果、取消、重启恢复、无 Jev/故障 Jev、插件冲突、BI 来源、预算/并发、主题/键盘/弹框。

<a id="docker-connections"></a>

### 37.1 复用已配置Docker连接与测试隔离

用户已确认：当前配置的MySQL、PostgreSQL、Redis均由Docker启动，可执行读写/DDL等测试；无需每条fixture操作重复询问。不凭此授权推断后来新增的其他连接也可破坏。

执行时按数据库类型从已保存连接选择并记录 ConnectionId/作用域/配置revision/服务身份；使用原配置的host/port/TLS/SSH/认证，不猜默认端口/DB、不在日志导出密码。Docker来源本轮是用户声明，没有运行docker inspect或连接测试。

数据范围：
- MySQL优先独立 `fluxdb_ai_e2e_<runid>` database；无创建库权限时明确选择已授权库中的专用表prefix，不能偷偷操作既有业务表。
- PostgreSQL在该连接指定database建立 `fluxdb_ai_e2e_<runid>` schema，验证schema/search_path和跨scope拒绝。
- Redis使用原配置DB/topology和 `fluxdb:ai:e2e:<runid>:` prefix；Cluster按实际能力。不用FLUSHDB/FLUSHALL清理；只SCAN/UNLINK本run keys。
- 测试manifest跟踪本run创建的表/数据/keys，DDL/cleanup由受控fixture执行服务完成；无需修改连接配置或重置Docker容器。

测试分两层：策略/存储单测保持隔离SQLite/mock；真实DB与GUI/MCP测试使用这些现有连接。为避免更改用户AI设置，默认创建**应用配置的一致性本地快照**作为GUI/CLI test root：保留连接ID/非敏感配置/AES密文及同一数据钥匙，不新增一套手填连接。实际网络仍连接原Docker服务。快照目录权限收紧，含密钥的配置不进证据/CI/模型上下文；测试后清理。

快照是测试隔离helper，不是新备份产品或旧数据迁移。使用SQLite一致性备份，不能活动WAL时只copy主文件；复制的数据钥匙只保留在受控本地test root，绝不出现在截图/JSON报告。UI→命令→工具→真实Docker断言+小图完整链路仍成立。

当前只更新设计，没有执行真实查询/写入/截图。

**参考与落点：**[FileStorage与平台root](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/lib.rs)、[connection_store](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/connection_store.rs)、[connection_secrets](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/connection_secrets.rs)、[Connector](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/connector.rs)；落测试harness/fixture与已有执行服务，不造旁路驱动。

<a id="claude-mcp-test"></a>

### 37.2 Claude Code CLI实测MCP

此前已核对本机Claude Code 2.1.66的version/help/mcp add选项；这是CLI选项核对，不是实际Server已实现或MCP任务已成功。

先纯协议测试 initialize/tools/list/call/cancel/schema/isError/stdout 清洁，再 Claude Code 消费测试。MCP Server 二进制、隔离 fixture、Profile 尚未实现，因此现在只能验证 CLI 选项，不能宣称下面已跑通。

#### 37.2.1 隔离配置，不改用户 MCP 配置

测试 harness 用平台临时目录创建 `mcp.json`，插入实际绝对二进制路径/root/profile ID，不由 MCP 客户端自动展开 `${...}`。示例为本机路径，发布配置由平台生成：

```json
{
  "mcpServers":{
    "fluxdb_test":{
      "command":"/Users/shining3d/fusuwei/code/fluxDB/target/debug/fluxdb-mcp",
      "args":["--transport","stdio","--server-profile","fixture-readonly","--config-root","/private/tmp/fluxdb-mcp-fixture"]
    }
  }
}
```

这里的 fixture 路径只是测试示例，实际 harness 使用 tempfile/平台 temp dir。直接复用用户当前已配置Docker MySQL/PostgreSQL/Redis及AES连接凭据的一致本地快照，不要求重新填写登录或另起容器；工具网络仍连接原服务。本run MySQL database/PG schema/Redis prefix由harness创建，秘密不写入mcp.json/CLI env/证据；Claude 自身身份通过已有登录或测试授权配置，不读取/复制用户秘密。

#### 37.2.2 只读 MySQL smoke

未来执行命令示例；先 build MCP 后由 harness 设置 TEST_ROOT，整个 shell cwd 是隔离可信 fixture，不在真实仓库创建 .mcp.json：

```sh
claude -p \
  --mcp-config "$TEST_ROOT/mcp.json" \
  --strict-mcp-config \
  --setting-sources "" \
  --disable-slash-commands \
  --no-session-persistence \
  --tools "" \
  --allowedTools "mcp__fluxdb_test__fluxdb_connections_list,mcp__fluxdb_test__fluxdb_search_objects,mcp__fluxdb_test__fluxdb_describe_object,mcp__fluxdb_test__fluxdb_query" \
  --max-budget-usd 1 \
  --output-format stream-json --verbose \
  -- "只使用 fluxdb_test MCP：找到测试销售库，查找订单表，确认真实字段，统计 paid 订单金额；最后返回来源工具、SQL和总额。不读取文件，不使用 Bash，不修改数据。"
```

`--tools ""` 限制的是内置工具；`--strict-mcp-config` 限制加载哪些 MCP Server；allowedTools 精确批准本用例工具。禁止 `--dangerously-skip-permissions`。`--setting-sources ""` 的行为需用当前 CLI 在隔离测试确认，不将配置隔离误解为取消 Claude 身份认证。

stdout 由 harness 流式解析/有界保存，不把完整 stream-json 一次塞进模型上下文。设置进程 deadline、总用例费用上限和最大工具调用数；认证/网络不可用标 blocked，不自动付费重试。`--max-budget-usd` 是单次运行限制，suite 还须累计限额。

#### 37.2.3 PostgreSQL smoke（当前连接）

使用相同read-only工具与CLI隔离旗标，选择用户已有PostgreSQL连接、本run schema；prompt要求描述真实字段后统计paid订单，并检查schema边界。SQL placeholder为`$1/$2`，不能照搬MySQL的`?`。

断言真实PG结果/精确numeric投影、参数真bind、search_path/显式schema、越过grant拒绝；测试schema来自已配置database，不猜默认postgres库/5432端口。Driver不支持某类型就明确Unsupported，不把numeric/u64悄悄转f64。

#### 37.2.4 Redis smoke

同样旗标，只授权 connections_list/redis_scan/redis_inspect/redis_server_info，prompt：“找到测试 Redis DB，扫描 `fluxdb:test:*`，查看一个 string 和 hash 的类型、TTL、有限值，说明截断/遍历范围，不写入”。

assert：实际调用了正确 MCP tools，而不是凭空输出；只读 fixture keys 未变；key prefix/DB 不越界；大值触发受控反馈；usage/进程退出和 MCP request 审计一致。

#### 37.2.5 写入/拒绝、资源隔离与错误测试

- Readonly Profile 隐藏 execute tools；直接协议调用也拒绝，实际数据不变。
- Scoped Profile 仅在一次性测试数据库放行明确的单条 UPDATE/SET 等；CLI 获准调用工具不应越过另一个库/Key prefix。用独立后端读取验证变化。
- 语法错误→真实 isError→Claude 可修正；max_rows/byte budget 超限不把截断当全量。
- 同 session 查询后 result_page，不重复 SQL；另一 session 同句柄拒绝；EOF/取消后句柄过期。
- stdout 混入日志、invalid schema、未知工具、profile disable、connection delete、revision change、未知写入、进程中止、回执窗口崩溃均测试。
- Request status 与结果正确还需真数据库断言；`claude mcp list`/“Connected”只能证明连接，不能证明 SQL/Redis工具正确。

#### 37.2.6 可选人工注册命令

若用户需要手动调用，在隔离测试项目目录：

```sh
claude mcp add --scope project --transport stdio fluxdb_test -- \
  "$FLUXDB_MCP_BIN" --transport stdio --server-profile fixture-readonly --config-root "$TEST_ROOT/storage"
claude mcp list
```

自动化优先 mcp-config，不永久改全局配置；本轮没有执行 add/list。`claude mcp serve` 是 Claude 自己对外提供 Server，不是本功能的测试替代。

**就近参考：**[已有Connector/真实执行](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/connector.rs)；[Docker fixture设计](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-design.md#docker-connections)。实际落点按本章/§7，不照搬源项目私有状态。

### 37.3 已配置真实模型验收与凭据使用

用户已授权验收时复用本机 SQLite `ai_model_profiles` 中的“公司自建”（文本生成主模型）和“决策模型”（Jev 决策模型），无需重新填写 API Key；这是测试数据选择规则，不是生产代码硬编码名称。执行前通过现有配置仓库读取非敏感模型信息，核对唯一模型 ID/revision、用途、启用状态、Provider/endpoint 与能力；重名、缺失或不兼容时明确受阻，不静默换模型。用户确认已配置不等于已验证连通或协议兼容。

- **凭据链路**：复用现有 storage 凭据读取/AES-256-GCM 解密能力及连接凭据共用的数据密钥，由宿主在请求前取得短生命周期凭据并直接交给 Provider transport。不得重写解密算法、绕过 AAD/版本认证或让模型/插件自行读取 SQLite/config.toml。明文只在必要的进程内请求边界使用，不进入 CLI 参数、环境变量、提示词、普通 DTO、日志、截图或报告；不得输出 ciphertext/nonce/数据密钥。解密失败标记 Unavailable 并停止该模型调用，不清空、覆盖或重新生成用户密钥。
- **隔离与限额**：按§37.1使用 SQLite 一致快照与受控本地密钥配置，沿用真实 endpoint/模型配置，不修改原模型、默认选择或插件开关；测试开关只作用于隔离配置。真实网络请求显式启用，普通 `cargo test` 不自动消费真实模型；T030保护就绪后执行，运行前记录请求/Token/迭代/时间上限与可用的费用上限，价格未知不记作免费，严格费用模式无法估算时受阻。只发送合成 fixture 和已授权测试数据，不发送业务库或秘密文件。
- **R01 主 Agent（公司自建）**：关闭 Jev，从真实窗口发起未知表探索→结构查询→汇总→结果解释，使用独立数据库查询核对数字、目标、SQL与来源；记录真实 Provider 请求、工具回执和用量，Jev 请求数必须为0。另测停止和受控错误后的修正；错误不一定由模型自然产生，可在测试服务中明确注入一次可恢复错误并标注，不伪造成功回执。不以回复流畅或固定工具调用次数判断通过。
- **R02 Jev（决策模型）**：主模型仍用“公司自建”，在隔离配置启用合格“决策模型”，执行会触发设计中决策点的多候选任务；核对真实 decision 请求、Choice/Score解析、所选结果与实际路由/过滤效果、用量记账，不能仅凭UI显示“已启用”判通过。真实协议不匹配标记受阻，不按生成模型协议冒充；无必要决策点的零调用不能证明Jev已验证。
- **R03 可选性与故障**：用同一fixture分别关闭/启用Jev比较目标和结果正确性，关闭时零decision请求；超时、非法返回、缺凭据及熔断用受控Provider注入，确认基础路径继续且无反复等待/弹错。真实成功链路与注入故障分开报告，不为故障测试破坏用户原始凭据。

证据只保留模型ID/revision、用例、脱敏请求关联ID、实际工具/后端断言、用量/耗时及当前构建截图路径。真实模型失败、能力不支持或限额中止必须如实记录；本节是待执行方案，本次文档修订未解密密钥或调用模型。

## 38. 小截图与逐步上下文压缩协议

**限制 decoded 像素和张数，不只是文件字节。**JPEG 很小的大图仍可能占巨大解码内存。测试程序原生截图完成后先本地裁剪/缩放/压缩并校验，再让视觉工具读取；禁止先把高 DPI 整屏图送入上下文。

建议默认：单图边界 ≤ 1280×800、像素 ≤ 1,000,000；优先目标区域/窗口裁剪，正常字号清晰。编码优先 JPEG quality 78 左右或支持的 WebP，文字细节用小块 PNG。目标 ≤ 250 KiB，硬上限 512 KiB。尺寸/像素/字节都是可配置的测试阈值，超过必须重新处理，不能绕过。压缩工具不支持某格式时用可验证的等价编码，不假装工具已安装。

只有当前步骤最多一张图进入视觉上下文；需要看细节时用该区域替换当前图，不同时添加整图与多份放大图。不能缩到字看不清以满足字节数；先裁剪，必要时拆成多个**串行**步骤。原始大图仅临时本地留存，不嵌入 Markdown、不上传、不用 base64 写进 JSON。若某 UI 工具会自动把原图送入模型且不能限定尺寸，就不能用该图像接口完成此测试；改用本地窗口捕获→处理→受限图展示路径，先验证工具能力，不能寄希望于上传后再压缩。

### 38.1 每一步操作协议

```text
读取上一步 checkpoint（纯文本）
→执行一个 UI 动作/有限动作组
→等待明确状态/事件条件（有 deadline，不盲目 sleep）
→核对 UI 状态 + 真实数据库/文件/usage/审计断言
→本地生成一张受限截图（必要时脱敏）并检查尺寸/字节
→只看这张图，记录可见结论
→保存 step.json、checkpoint.md、断言结果与截图 hash
→结束当前步骤；下一步只读取检查点和当前截图
```

checkpoint 包含：用例/step、应用构建与平台、fixture、当前 session/turn、目标 scope、完成动作、可见/后端断言、重要文本、待验证项、下一步、证据相对路径/hash。不包含 api key、密钥、加密载荷、整图 base64、全部历史工具正文。

**保存文本不等于宿主模型真的压缩了上下文。**在支持显式压缩/重启评测轮次的测试执行器中，每步结束使用其真实能力；否则每步新评测调用只输入 checkpoint+当前图。不要宣称能由插件命令强制当前 Codex 对话内部压缩。fluxDB Agent 自己的图片输入使用同样的媒体卸载/引用与活动上下文预算。

### 38.2 截图处理伪代码

```python
# 拟新增测试 helper；原图不 emit，不先交给视觉模型。
img = capture_window_locally()
img = redact_secret_regions(img)
img = crop_to_relevant_panel(img)
img = resize_preserving_aspect(img, max_width=1280, max_height=800,
                              max_pixels=1_000_000)
for quality in (82, 78, 72):
    encoded = encode_jpeg(img, quality=quality)
    if len(encoded) <= 250 * 1024:
        break
assert img.width * img.height <= 1_000_000
assert len(encoded) <= 512 * 1024
# 文字不可读：不再降清晰度，改为较小区域的下一串行截图。
write_evidence(encoded)
```

JPEG quality 只是起始候选；必须实际目视验证。截图外字段只用文字/语义树确认时注明证据类型，不能将纯日志冒充 UI 验证。

**就近参考：**[本仓库UI规范](/Users/shining3d/fusuwei/code/fluxDB/docs/ui-style.md)。实际落点按本章/§7，不照搬源项目私有状态。 图片像素/字节/串行检查点是本项目测试规范，源项目并未替我们实现宿主上下文压缩。

## 39. 端到端用例的逐步脚本

| 用例 | 串行步骤与后端断言 | 最小截图证据 |
|---|---|---|
| E01 模型管理与加密 | 新建→Set→重启→Keep 改名→Clear；查询密文列 shape、实际 decrypt、CAS、known secret 不在日志/DTO | 表单占位/状态；不截真实 Key |
| E02 无 Jev SQL 任务 | 不建 decision model→@连接→要求未知表汇总→真实查询→结果 | 工具时间线、结果数字；decision 调用数=0 |
| E03 SQL 修正 | scripted 返回错误字段→收到真实数据库错误→修正→成功 | 错误与修正条目；正确总量 |
| E04 执行目标 | 开始库 A 任务→切换到 B→继续→尝试修改目标 | A 的实际查询日志；审批目标清晰，B 不受影响 |
| E05 写审批 | 生成 UPDATE→拒绝→数据不变；重新生成→批准→数据变化；参数改动旧批准失效 | 审批/拒绝/回执，数据库 before/after |
| E06 Jev 可选 | 启用正常→禁用→超时→非法结果→无凭据→熔断→完成任务 | 状态/一次诊断，不能每轮弹错误 |
| E07 压缩 | 多轮大结果→触发→检查保护条目→继续完成；截断摘要不得提交 | 压缩条目、后续正确目标/口径 |
| E08 Redis | SCAN→inspect→请求危险操作→拒绝/批准测试 DB | scan cursor 与范围、风险审批；实际 key 状态 |
| E09 文件/@ | 同名对象选区→文件→编码/大文件→越权 symlink→拒绝 | chip/错误，授权路径证据 |
| E10 BI 插件 | 读取业务口径→真实数据→图表→报告→导出→卸载 renderer | 图表/来源；导出内容/hash/缺插件说明 |
| E11 停止和重启 | 长读停止→等待审批重启→中断写 unknown→禁止重放 | stopped/unknown，实际调用计数 |
| E12 Hook 与预算 | 参数 patch→重新审批；deny 优先；并发预留；未知价格 | timeline、额度提示，ledger 验证 |
| E13 UI 完整性 | 明暗主题、键盘导航、Esc/遮罩、菜单点击、防穿透、loading | 每个主题按需一小图，不能整屏拼长图 |
| E14 内置插件/预留接口 | auto启用Jev、手动禁用、必经审计、未知外部runtime拒绝 | 自动状态/审计/能力，不测试未实现安装器 |

真实模型再运行 E02/E03/E10 和跨工具开放任务，允许不同路径但使用语义断言。报告成功率/无依据结论/人工介入/耗时/费用；不固定“必须调用 N 次工具”来限制智能。

<a id="mcp-ui-e2e"></a>

### 39.1 CLI、GUI、后端与小截图联合验收

CLI 测试不必为了截图输出终端整屏长图。实际界面串行步骤：

1. 主窗口建立只读 Server Profile 和 MySQL/PostgreSQL/Redis授权资源，截图当前设置小区域。
2. 导出不含秘密的 MCP 配置，文件 schema/路径断言；小图只证明导出状态。
3. harness 运行一条 Claude CLI 任务，解析 tool-use/result；有界文本 checkpoint，不截图海量 stream。
4. 主窗口 MCP 活动页显示相同 profile/session/request、工具、资源、成功/拒绝/unknown；截活动条目小区域。
5. 独立后端验证查询/写入事实，保存脱敏断言；仅在需要时截结果小区域。
6. 禁用 Profile，再调用应拒绝；检查 UI 状态、server 审计和真实数据不变。

每一步遵守本文§38：单图≤1MP/512KiB，目标≤250KiB，最多当前一张，checkpoint+当前图进行下一评测，不上传 raw 大图。CLI 与 GUI/后端三方 ID/hash 对齐，不能用一段 Claude 文本当界面 E2E 完成。

<a id="ui-persistence-e2e"></a>

### 39.2 菜单、持久化、Jev与Docker验收扩展

- U01：设置“大模型/工具与MCP”菜单、空状态、默认模型/Jev自动状态、按钮/键盘/主题。
- U07：首次抽屉→拖宽/窄→切内嵌→关闭重开/重启后再次打开保持内嵌；两宽各自恢复，W=1200/1600/700与高DPI下严格60%/40%，窄窗口不换模式。
- U08：打开恢复上次查看会话；新建不生成空历史/不清旧任务，未发送草稿安全；关闭运行/等待批准只隐藏，不停止/批准/重放；形式切换Session与正文/焦点/草稿/锚点不丢。
- U02：公开思考摘要/计划/工具卡/审计/报告美观与一致；无reasoning能力不编造；单图≤1MP/512KiB，目标≤250KiB。
- U03：流式消息、手动折叠、上滚不抢焦点、错误/批准/unknown/停止不同状态；500条历史分页不全加载。
- U04：正常退出/强杀/重启重建message parts/事实/展开状态，未完成part标interrupted，无写重放。
- U05：未配置Jev→保存启用合格模型→自动可用→禁用/Key清除/异常→基础路径继续；多模型待选择，不自动花高费用。
- U06：同一已配置Docker MySQL/PostgreSQL/Redis从UI与Claude MCP执行本run fixture，核对实际数据、audit hash、target revision和截图对应。

测试一步只输入当前checkpoint+当前小图；日志与原始大图本地有界，不全量送进评测上下文。这里列的是待执行验收，不是本轮测试结果。

**就近参考：**[GUI/CLI/后端步骤规范](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-design.md#mcp-ui-e2e)。实际落点按本章/§7，不照搬源项目私有状态。

### 39.3 UI 测试执行方法与还原度验收

每个 UI 任务按以下顺序验收，范围对应本章 E/U 用例；以下是执行要求，不代表测试基础设施已接入或用例已通过。

0. **测试入口（实现现状）**：`NavicatMain` 已由模块级 `navicat_main_new(window, cx, controller, storage, theme_mode)` 构造，生产启动与 `#[gpui::test]` 共用同一装配路径；测试用隔离临时存储与 `VisualTestContext`（`simulate_click` / `simulate_input` / `capture_screenshot`），不通过直接调用处理函数替代交互。

1. **确定基准与场景**：打开 [AI Agent 交互稿](prototype/ai-agent-ui.html)，结合本设计与 [UI 样式规范](ui-style.md)，列出受影响页面、按钮及预期结果。原型只作视觉/交互基准，原型的浏览器测试或 `ai-agent-ui-test-report.json` 不能证明原生应用通过。
2. **GPUI 交互回归**：先核对锁定的 `gpui-pre` 测试接口，在测试依赖中启用 `test-support`；用 `#[gpui::test]`、`TestAppContext` / `VisualTestContext` 创建被测视图，模拟鼠标点击、输入、快捷键、焦点切换和弹框关闭，断言状态及发出的业务命令。用受控异步响应验证 loading/disabled、成功、空态和错误态；不以直接调用按钮处理函数代替点击。通过 `cargo test -p fluxdb-desktop <测试名过滤器>` 执行，过滤器替换为实际测试名并确认匹配用例非零；进程内测试不替代真实窗口 E2E。
3. **真实窗口端到端**：按§37准备隔离配置与测试数据，运行 `cargo run -p fluxdb-desktop`，用桌面自动化或人工实际点击入口→输入→提交→检查结果，按§38等待明确状态且设置超时。覆盖受影响的保存/取消、菜单、Esc/遮罩关闭、防穿透、审批/拒绝、停止与重启恢复；涉及写入必须检查真实后端 before/after 和审计，不能仅看成功提示。错误/慢响应通过测试 fixture 或受控服务构造，不改生产行为；真实模型与数据库验证仍遵循本章原有要求。
4. **截图检查 UI 还原度**：使用系统或桌面自动化工具截取真实应用窗口/区域，按§38先裁剪、缩放、压缩再读取；不强制引入 `gpui-preview`。在相同逻辑窗口尺寸、主题和业务状态下对照原型，检查区域比例、对齐/间距、字号/换行/截断、颜色/图标、按钮与输入框及各状态；覆盖明暗主题、正常/窄窗口，面板宽度与高 DPI 按 U07 验证。允许原生字体渲染差异，不要求 HTML 与 GPUI 逐像素一致，但裁切、遮挡、错位、不可读及交互状态缺失必须修复。
5. **复测与记录**：按模块修复后复跑失败用例及相关回归，记录“用例/平台与窗口尺寸/操作/预期与实际/结论/必要截图路径”；分别标明 GPUI、真实窗口 E2E、视觉验收结果。截图只是视觉证据，不能替代交互及后端断言。共享 GUI 的测试串行执行；无法执行的项写明阻碍，不宣称通过。

### 39.3.1 UI稿位置、对比步骤与任务归属

原型唯一入口：仓库相对路径 `docs/prototype/ai-agent-ui.html`，当前工作区绝对路径 `/Users/shining3d/fusuwei/code/fluxDB/docs/prototype/ai-agent-ui.html`；配套 `ai-agent-ui-pages.js`、`ai-agent-ui.js`、`ai-agent-ui.css` 定义页面、交互和样式。绝对路径仅用于本机定位，测试工具应从仓库根目录解析路径，不硬编码用户目录。

| 所属任务 | 原型入口与本任务负责的内容 |
|---|---|
| T013 | `#models`：设置大模型入口、模型表格/搜索、默认模型选择器、新建/编辑弹框、测试连接与凭据状态 |
| T014 | `#agents`、`#tools`、`#plugins`：Agent列表及三分区编辑、保存栏、工具列表与插件开关；工具与MCP入口和页签容器也在此交付，Server/审计内容由T060/T065交付 |
| T020 / T022 | `#workbench`→“询问AI”：审批卡的批准/拒绝；输入`@`及附件选择、候选、chip与移除 |
| T028 / T029 | `#workbench`→“询问AI”、`#history`：抽屉/内嵌、宽度与记忆、会话入口、发送/停止、时间线/折叠/滚动、恢复与unknown状态；存储语义由T015支撑 |
| T033 / T035 / T037 | `#models`、`#plugins`及询问AI：模型选择、摘要进度与失败反馈、Jev自动状态/开关与降级 |
| T040 | 询问AI产物卡→报告弹框：图表、来源、SQL、导出；不把报告放进设置页 |
| T060 / T065 | `#mcp`、`#audit`：本地Server、资源授权、配置导出、活动与安全审计 |
| T064 | 上述已分别通过的页面之间导航、状态一致性与完整任务回归；不承接前置任务欠缺的控件或视觉修复 |

对比步骤：①浏览器打开原型并通过按钮进入目标页面/弹框/场景，不能只阅读HTML源码；②真实应用准备语义等价的测试数据、主题、逻辑窗口尺寸和控件状态，记录缩放/DPI；③分别截取相同区域，按§38处理后串行查看原型与实机小图，逐项记录“预期→实际→差异”，不能只看实机截图说“正常”；④核对结构/字段/按钮完整性，再核对比例、间距、字号、颜色、溢出与状态；⑤修复后重跑受影响操作并更新实机截图。对比不要求模拟数据文本或原生字体抗锯齿逐像素相同，但不能自行删减控件；原型与设计冲突时先记录并确认，不默默择易实现。原型未覆盖的状态按正文与UI规范验收并注明，不能伪造原型证据。

### 39.4 UI 任务通过标准（完成门槛）

每个 UI 任务在实现前列出范围内的“原型页面/状态 → 控件与行为 → 用例 → 证据”，按§39.3执行，并逐项满足：

| 验收项 | 通过标准 |
|---|---|
| 功能完整性 | 设计要求的入口、字段、列表/表格列、选择器和操作齐全且可用；不得用纯文本、空按钮或占位页替代交互控件。只读/禁用须符合设计并说明原因。 |
| 数据展示闭环 | 按§39.5验证真实加载与保存后的数据经应用状态、渲染快照到实际控件；有数据场景必须看到目标记录，不能只验证数据库或孤立组件。 |
| 交互回归 | 受影响的点击、输入、键盘和弹框行为有实际执行且通过的测试；断言状态与业务结果，相关 loading/disabled、空态、失败与恢复均覆盖。仅启用 `test-support`、测试编译成功或零用例不算通过。 |
| 真实窗口 E2E | 从真实入口操作到最终结果，相关保存/重启恢复、审批/拒绝等流程通过；涉及后端变化有独立结果核对。直接调用处理函数、调试入口直达页面或截图不能替代完整流程。 |
| 视觉还原度 | 实际运行截图已对照原型检查结构、控件、布局间距、文字和颜色；明暗主题、正常/窄窗口均无漏项、遮挡、溢出和不可读。原生字体渲染差异可注明，不要求逐像素一致；结构/交互改变不能自行当作平台差异放行。 |
| 修复闭环 | 范围内已发现的功能与视觉差异已修复，并对修改后的构建重新测试、截图；只有差异清单或修改前截图不算通过。 |

**状态与证据：**分别记录“GPUI交互 / 真实窗口E2E / 视觉还原度”的通过、失败、受阻或不适用（附理由），注明构建版本、平台、场景及证据路径；不适用不能用来规避本任务必需的验收。缺少测试工具、fixture或可测试构造入口时记录阻碍及下一步，状态为“实现部分完成／验收受阻”，不勾选完成。实施验收遵守本文章节2的平台验收分工。

**勾选规则：**本任务必需项全部通过才可勾选；记录问题不等于修复问题，补写测试报告不等于复测。范围内缺口不得转交后续任务以换取当前任务完成；需调整设计或延期时先由用户确认并同步范围。已勾选但证据缺失或仍有必需项未通过的任务，应撤回勾选、保留历史记录并补验收。T064仅负责跨模块整体回归；原有功能实现已按§39.3.1归回所属任务，不替代前置UI任务各自的验收。

### 39.5 数据到页面的闭环验收

所有数据驱动页面必须验证生产实际链路：`持久化/服务返回 → AppState → 渲染快照/视图模型 → 实际列表、卡片或表单`；新增/编辑还须从真实UI操作经`AppCommand → 保存 → 状态更新/重新加载`接入该链路。数据库写入成功、状态包含记录、成功提示和空页面截图都不能单独证明功能完成；只向孤立列表组件手工塞数据也不能替代集成验收。

每个相关任务按已设计支持的操作执行以下场景，不为测试新增产品功能：

1. **已有数据首次展示**：隔离fixture预置具有唯一标记和已知ID的记录，从正常入口打开页面，断言生产快照保留该记录、实际控件显示关键字段与预期数量；过滤/分页影响数量时按当前条件核对，不能把未进入当前页误判为丢失。
2. **保存后即时展示**：通过UI新增或编辑后，等待有deadline的状态完成条件，无需用户手动刷新/重启，目标记录出现或原行更新且不重复；独立读取后端确认ID、字段与页面一致。删除/启停仅在功能支持时验证，操作失败不能伪装成功或丢失已有数据。
3. **返回与重启恢复**：离开页面再返回、重启后重新进入，仍显示已保存记录及状态；会话、审批与审计按各自事实恢复，不触发操作重放。
4. **四态分离与异步竞争**：受控fixture分别构造loading、成功空数据、成功有数据和加载失败；失败明确显示错误/重试，不以空集合吞掉错误；按设计恢复后展示真实数据。切换对象/页面后旧请求迟到不得覆盖新页面或新保存结果。

**测试分层**：自动化回归至少经过生产的加载、状态到快照转换及视图构造，不复制一份转换逻辑到测试中；为“状态有记录但快照丢记录”添加断言，再由GPUI模拟输入和真实窗口流程确认记录实际可见。外部服务可受控替换，但不得绕过被测数据绑定路径。已有数据、保存后及恢复场景可复用fixture，按需保留最少有数据状态截图。

**证据与门槛**：记录用例、唯一测试标记/非敏感ID、预期与实际字段/数量、后端断言、自动化测试结果及真实页面截图路径；不得包含凭据。仅检查SQLite或仅截图空态不通过；数据已保存却不可见属于所属任务功能缺陷，修复并复测后才可勾选，不交给T064兜底。

## 40. 交付阶段与发布门槛

- M0：契约、就近参考、首次schema/crypto、已有Docker连接fixture、脚本测试入口。
- M1：模型/Agent/提示词/插件实例等独立表管理与凭据；界面可配置。
- M2：内置 runtime、无 Jev 路径、`@`/UI 上下文、受控 SQL/Redis/文件工具、审批/停止/恢复。
- M2-MCP：复用 M2 工具提供 headless 本地 Server、Profile UI、独立审计和 Claude Code CLI 测试；不依赖Jev/未来第三方。
- M3：路由/压缩/Hook、可选 Jev、业务知识与 BI 报表插件。
- 本期边界：只交付内置插件与第三方接口；MCP Client、MongoDB/HTTP/SSH、安装/沙箱/市场另列未来，不是本期验收门槛。

M2/M3 是用户核心体验目标，不能以只有管理页和聊天接口宣布完成。本期不交付第三方安装，不把内置能力或预留接口冒充热安装；发布文档如实标明。

Rust 修改后 cargo fmt/check，受影响 crate 测试；UI 行为修改运行主窗口并做对应 E2E。文档阶段只做链接/DDL/设计一致性检查，不伪造 cargo/GUI 验证。交付要求当前开发平台启动、核心 E2E、无凭据泄漏、无 Jev 全套通过；其他未验证范围必须列明，平台验收遵守§2的分工。

<a id="tool-mcp-delivery"></a>

### 40.1 工具与MCP实施映射及完成门槛

T054–T063为MCP专项，T064–T066补设置/审计/当前Docker验收，见任务清单M2-MCP阶段；T045 明确只管可选 MCP Client。外部 Server 可以先在没有任何模型/Jev配置下交付，不依赖未来第三方插件安装/Wasm；复用 M2 工具/安全服务即可。

完成条件：typed tool contract/参数真 bind/Redis decode budget、只读/受限修改权限、headless stdio、独立 Server 表与审计、Claude CLI 实际消费、对应 UI 小截图和后端断言；实施阶段在当前开发平台测试。仅本轮设计和 CLI help 核对不满足这些实现门槛。

**就近参考：**[当前/延期任务边界](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-tasks.md)。实际落点按本章/§7，不照搬源项目私有状态。

## 41. 关键决策与实现前待验证项

已定：原生内部 runtime、插件能力分层、Jev 可选、独立表、模型行密文、对象 CAS、权限内核、结果来源、逐步小截图。

需在原型任务验证而不是再向用户泛问：Rig Provider/hook 与自有 runtime 是否可组合；生产异步 HTTP 依赖；只读/取消实际三种数据库支持；message parts与公开reasoning能力；图表组件及 PDF/XLSX 导出能力；测试主窗口隔离 root 的最小接入点。

不得先实现再隐藏差异：任何无法达到的安全/平台/取消能力通过 Unsupported 或清晰 UI 提示表达。设计变更更新本文与任务验收，特别是 §42 的 DDL/DTO/CAS，不再产生第二份相互矛盾 schema。

**就近参考：**[原型与验收任务](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-tasks.md)。实际落点按本章/§7，不照搬源项目私有状态。

## 42. AI 独立表与对象级存储契约

本章与 AGENTS.md 的“设计文档 §42”对应。§42.10/§42.11 是完整基线 DDL；未来实现以 [schema.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/ai_platform/schema.rs) 为源码唯一来源，本期仅首次幂等建表，无旧AI迁移；未来真正升级时再设计版本转换，不能只改IF NOT EXISTS蒙混结构差异。

### 42.1 统一约定

- AI ID：应用分配 UUID 文本；ConnectionId 保持现有 INTEGER，不把数据库连接迁移成 UUID。
- 时间：UTC epoch milliseconds INTEGER；UI 再按用户时区展示。金额：micro-USD INTEGER；token/字节/次数：非负 INTEGER。
- 管理对象 revision 从 1 开始；成功修改递增，WHERE 包含 expected_revision。secret_revision 从 0 开始，每次 Set/Clear 递增。
- JSON 列均 json_valid，并在应用层做 schema/version/大小校验；不能把其他可独立管理实体的正文藏到 JSON。
- 每个连接 foreign_keys=ON；约束错误映射到可理解的应用错误，不吞掉。引用资源的启用/兼容/发布状态另在 app 校验。
- 模型秘密只在 ai_model_profiles；无独立 ai_credentials。ciphertext 是 AES-GCM 密文连同 tag；nonce 12 bytes。

### 42.2 七类核心独立管理对象（提示词属于Agent）

| 表 | 行的含义 | CAS / 主要引用 |
|---|---|---|
| ai_model_profiles | 一个生成或决策模型配置及私有密文 | revision + secret_revision；被 Agent/路由引用 |
| ai_agents | 一个 Agent 配置 | revision；模型、Agent内提示词、预算 |
| ai_plugin_instances | 一份插件实例配置 | revision；引用 package/version/digest |
| ai_agent_plugin_bindings | 一个 Agent 与实例的能力绑定 | revision；顺序、范围、错误策略 |
| ai_budget_policies | 一个预算策略 | revision；session/day/调用限制 |
| ai_mcp_settings | 一个 MCP server 设置 | revision；可选，不是整个全局 JSON |
| ai_runtime_settings | 一份应用级 AI 运行设置 | singleton id=1 + revision；仍独立表 |

辅助管理表：ai_agent_model_bindings、ai_agent_mcp_bindings、ai_mcp_server_profiles、ai_mcp_server_resource_grants、ai_plugin_packages、ai_integration_secrets、ai_knowledge_entries；公共模型/MCP 引用独立建列与 FK，不能藏在 plugin JSON。package/integration secrets 用于可安装插件/MCP 阶段，不能把它们借机做模型密钥存储。

### 42.3 模型 API Key 私有行与公共 DTO

公共 ModelProfile：id、display_name、provider、api_protocol、model_kind、model、endpoint、enabled、auth_mode、capabilities、provider_options、上下文窗口Token、最大输出Token、计价（输入/输出/缓存读取micro-USD每百万）、revision、CredentialStatus。模型 endpoint/Provider 改动若将 Keep 的原 Key 发送到新 origin，保存前必须明确确认新的出站目标；路由插件不能修改 endpoint 或直接提供 HTTP URL。没有 ciphertext/nonce/key_id/cipher_version/secret_revision 字段。

私有 EncryptedModelRow：只在 storage 内读取全部加密列；不 derive Serialize，Debug 脱敏。模型 RuntimeSnapshot 同样是公共配置投影，不能克隆 private row。凭据请求通道只允许 Provider transport 内部使用。

普通 export SELECT 必须列举白名单列，不使用 SELECT * 再删掉 secret 字段。导入只接受公共 DTO，拒绝注入 encryption metadata。

### 42.4 对象级 CAS 示例

```sql
UPDATE ai_agents
SET display_name = :display_name,
    options_json = :validated_options,
    revision = revision + 1,
    updated_at_ms = :now
WHERE id = :id AND revision = :expected_revision;
```

affected_rows=0 时查询 ID 是否存在，区分 NotFound/Conflict。更新 UI 只在 commit 后发生。多实体操作（保存Agent与其直接提示词、system内置实例自动装配）在短事务内逐对象检查 expected_revision；不是先 save-all 再覆盖。

### 42.5 运行快照与引用完整性

Turn 保存脱敏且有 schema_version 的 runtime_snapshot：Agent/模型/package/binding/budget的ID+revision、直接提示词快照/hash、固定目标、能力和策略。模型 key 仅在实际请求时获得，凭据变化/禁用会阻止下一次使用，不用旧快照绕过撤销。

模型/Agent/Agent配置被历史引用时 disable/archive，不默认级联删除。历史快照是执行事实，禁用配置不修改旧快照。安全策略收紧和授权撤销即时作用于下一工具边界。

### 42.6 会话、消息、工具、审批

ai_sessions 管长期会话；ai_turns 管任务；ai_messages存message头/metadata，ai_message_parts存正文/公开摘要/引用与partial checkpoint；ai_session_ui_state存显示状态；ai_tool_calls 存准备/执行/未知状态；ai_approvals 存审批 hash 与决定；ai_run_events 仅保存有意义的脱敏事件。

messages/session seq、parts/message ordinal、events/turn seq分别唯一；分配 seq 与 INSERT 同事务。tool call ID 使用应用 ID，Provider call ID 按 iteration 保存，避免不同 Provider 重用 `call_0` 冲突。

### 42.7 结果、产物与保留

ai_results 保存结果 metadata/受控 spill 文件位置/保留与过期状态；默认内存结果重启后 expired，不能假设 metadata 可重建数据。保留 spill 有用户策略与空间上限。

ai_artifacts 保存 spec/状态/路径/hash；ai_artifact_sources 正规化记录来源，支持追溯和删除影响查询。文件位置是相对受控根，不能直接传给第三方插件做任意读写。

### 42.8 预算记录

ai_usage_ledger 每次实际请求/attempt 一行，包含 main/decision/compaction/artifact 类型、token 与费用的 actual/estimated/unknown 标记。ai_budget_reservations 每次请求预留；并发预留和余额检查同事务，计算 settled 实际量+held/unknown，不重复扣同一 request attempt。

### 42.9 首次建表与内置能力种子

先创建 schema，再创建基础预算/运行设置/内置插件 package manifest 与实例；没有APIKey/默认付费模型的种子配置；默认内置插件由代码注册/system绑定自动建立，安全审计不可关闭，Jev没模型时idle不报错。必须幂等建立可编辑但不可删除/停用的内置默认 Agent；用户配置主模型后即可运行。无 Jev 的种子数据完整工作。

下面 DDL 分两个代码块，按 §42.10→§42.11 顺序执行；SQLite 允许引用后续定义的表，但**所有表创建完成后**才插入种子。本期对同名不匹配表明确报错，不自动转换旧数据。

### 42.10 管理实体完整基线 DDL

```sql
CREATE TABLE IF NOT EXISTS ai_model_profiles (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    provider TEXT NOT NULL,
    api_protocol TEXT NOT NULL,
    model_kind TEXT NOT NULL CHECK (model_kind IN ('generation','decision')),
    model TEXT NOT NULL,
    endpoint TEXT,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    auth_mode TEXT NOT NULL CHECK (auth_mode IN ('none','api_key')),
    context_window_tokens INTEGER CHECK (context_window_tokens IS NULL OR context_window_tokens >= 1024),
    max_output_tokens INTEGER CHECK (max_output_tokens IS NULL OR max_output_tokens >= 256),
    capabilities_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(capabilities_json)),
    provider_options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(provider_options_json)),
    input_price_micros_per_million INTEGER CHECK (input_price_micros_per_million >= 0),
    output_price_micros_per_million INTEGER CHECK (output_price_micros_per_million >= 0),
    cached_input_price_micros_per_million INTEGER CHECK (cached_input_price_micros_per_million >= 0),
    price_metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(price_metadata_json)),
    credential_ciphertext BLOB,
    credential_nonce BLOB,
    cipher_version INTEGER,
    key_id TEXT,
    secret_revision INTEGER NOT NULL DEFAULT 0 CHECK (secret_revision >= 0),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK (
      (credential_ciphertext IS NULL AND credential_nonce IS NULL
       AND cipher_version IS NULL AND key_id IS NULL)
      OR
      (credential_ciphertext IS NOT NULL AND typeof(credential_ciphertext)='blob'
       AND length(credential_ciphertext) >= 16
       AND credential_nonce IS NOT NULL AND typeof(credential_nonce)='blob'
       AND length(credential_nonce)=12 AND cipher_version IS NOT NULL
       AND cipher_version >= 1 AND key_id IS NOT NULL AND length(key_id)>0
       AND secret_revision >= 1)
    )
);
CREATE INDEX IF NOT EXISTS ai_models_kind_enabled ON ai_model_profiles(model_kind, enabled);

CREATE TABLE IF NOT EXISTS ai_budget_policies (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    session_limit_micros INTEGER CHECK (session_limit_micros >= 0),
    daily_limit_micros INTEGER CHECK (daily_limit_micros >= 0),
    request_limit_micros INTEGER CHECK (request_limit_micros >= 0),
    max_iterations INTEGER NOT NULL DEFAULT 40 CHECK (max_iterations > 0),
    max_wall_time_ms INTEGER NOT NULL DEFAULT 600000 CHECK (max_wall_time_ms > 0),
    max_model_concurrency INTEGER NOT NULL DEFAULT 2 CHECK (max_model_concurrency > 0),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);


CREATE TABLE IF NOT EXISTS ai_agents (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    default_model_profile_id TEXT REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    system_prompt TEXT NOT NULL DEFAULT '',
    budget_policy_id TEXT REFERENCES ai_budget_policies(id) ON DELETE RESTRICT,
    allowed_tools_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(allowed_tools_json)),
    permissions_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(permissions_json)),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_plugin_packages (
    id TEXT PRIMARY KEY,
    package_key TEXT NOT NULL,
    version TEXT NOT NULL,
    runtime_kind TEXT NOT NULL CHECK (runtime_kind IN ('builtin','wasm')),
    digest TEXT NOT NULL,
    host_api_min INTEGER NOT NULL CHECK (host_api_min >= 1),
    host_api_max INTEGER NOT NULL CHECK (host_api_max >= host_api_min),
    manifest_json TEXT NOT NULL CHECK (json_valid(manifest_json)),
    relative_path TEXT,
    status TEXT NOT NULL CHECK (status IN ('staged','active','disabled','failed')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(package_key, version, digest),
    CHECK (runtime_kind <> 'wasm' OR relative_path IS NOT NULL)
);
CREATE TABLE IF NOT EXISTS ai_plugin_instances (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    package_id TEXT NOT NULL REFERENCES ai_plugin_packages(id) ON DELETE RESTRICT,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    config_schema_version INTEGER NOT NULL DEFAULT 1 CHECK (config_schema_version >= 1),
    config_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config_json)),
    granted_permissions_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(granted_permissions_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_agent_plugin_bindings (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    plugin_instance_id TEXT NOT NULL REFERENCES ai_plugin_instances(id) ON DELETE RESTRICT,
    binding_source TEXT NOT NULL DEFAULT 'user' CHECK (binding_source IN ('user','system')),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    priority INTEGER NOT NULL DEFAULT 100,
    capabilities_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(capabilities_json)),
    failure_policy TEXT NOT NULL DEFAULT 'best_effort'
        CHECK (failure_policy IN ('best_effort','required')),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(agent_id, plugin_instance_id)
);
CREATE INDEX IF NOT EXISTS ai_bindings_order ON ai_agent_plugin_bindings(agent_id, enabled, priority, id);

CREATE TABLE IF NOT EXISTS ai_mcp_settings (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    transport TEXT NOT NULL CHECK (transport IN ('stdio','http')),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    config_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config_json)),
    tool_filter_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tool_filter_json)),
    permissions_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(permissions_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_mcp_server_profiles (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    transport TEXT NOT NULL DEFAULT 'stdio' CHECK (transport='stdio'),
    write_mode TEXT NOT NULL DEFAULT 'disabled'
        CHECK (write_mode IN ('disabled','scoped','desktop_approval')),
    allowed_tool_ids_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(allowed_tool_ids_json)),
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    max_inflight INTEGER NOT NULL DEFAULT 4 CHECK (max_inflight > 0),
    max_request_ms INTEGER NOT NULL DEFAULT 30000 CHECK (max_request_ms > 0),
    max_request_bytes INTEGER NOT NULL DEFAULT 262144 CHECK (max_request_bytes > 0),
    max_response_bytes INTEGER NOT NULL DEFAULT 131072 CHECK (max_response_bytes > 0),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_mcp_server_resource_grants (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES ai_mcp_server_profiles(id) ON DELETE RESTRICT,
    connection_id INTEGER NOT NULL,
    database_name TEXT NOT NULL,
    namespace TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    allow_read INTEGER NOT NULL DEFAULT 1 CHECK (allow_read IN (0,1)),
    allow_write INTEGER NOT NULL DEFAULT 0 CHECK (allow_write IN (0,1)),
    object_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(object_policy_json)),
    write_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(write_policy_json)),
    max_rows INTEGER NOT NULL DEFAULT 200 CHECK (max_rows BETWEEN 1 AND 1000),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(profile_id, connection_id, database_name, namespace),
    UNIQUE(profile_id, id)
);
CREATE INDEX IF NOT EXISTS ai_mcp_server_grants_profile
    ON ai_mcp_server_resource_grants(profile_id, enabled);

CREATE TABLE IF NOT EXISTS ai_agent_model_bindings (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    model_profile_id TEXT NOT NULL REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK (role IN ('main_candidate','compaction','artifact','decision')),
    binding_source TEXT NOT NULL DEFAULT 'user' CHECK (binding_source IN ('user','system')),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    priority INTEGER NOT NULL DEFAULT 100,
    options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(agent_id, role, model_profile_id)
);
CREATE INDEX IF NOT EXISTS ai_agent_models_role
    ON ai_agent_model_bindings(agent_id, role, enabled, priority);
CREATE TABLE IF NOT EXISTS ai_agent_mcp_bindings (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    mcp_setting_id TEXT NOT NULL REFERENCES ai_mcp_settings(id) ON DELETE RESTRICT,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    tool_filter_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tool_filter_json)),
    permissions_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(permissions_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(agent_id, mcp_setting_id)
);

CREATE TABLE IF NOT EXISTS ai_runtime_settings (
    id INTEGER PRIMARY KEY CHECK (id=1),
    default_agent_id TEXT REFERENCES ai_agents(id) ON DELETE RESTRICT,
    default_decision_model_profile_id TEXT REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    max_active_turns INTEGER NOT NULL DEFAULT 2 CHECK (max_active_turns > 0),
    context_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(context_policy_json)),
    privacy_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(privacy_policy_json)),
    retention_policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(retention_policy_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_panel_preferences (
    id INTEGER PRIMARY KEY CHECK (id=1),
    preferred_mode TEXT NOT NULL DEFAULT 'drawer' CHECK (preferred_mode IN ('drawer','embedded')),
    drawer_width_dp REAL NOT NULL DEFAULT 460.0 CHECK (drawer_width_dp > 0 AND drawer_width_dp <= 100000),
    embedded_width_dp REAL NOT NULL DEFAULT 420.0 CHECK (embedded_width_dp > 0 AND embedded_width_dp <= 100000),
    last_session_id TEXT REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    pending_draft_json TEXT CHECK (pending_draft_json IS NULL OR json_valid(pending_draft_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_integration_secrets (
    id TEXT PRIMARY KEY,
    plugin_instance_id TEXT REFERENCES ai_plugin_instances(id) ON DELETE CASCADE,
    mcp_setting_id TEXT REFERENCES ai_mcp_settings(id) ON DELETE CASCADE,
    secret_kind TEXT NOT NULL,
    ciphertext BLOB NOT NULL CHECK (typeof(ciphertext)='blob' AND length(ciphertext) >= 16),
    nonce BLOB NOT NULL CHECK (typeof(nonce)='blob' AND length(nonce)=12),
    cipher_version INTEGER NOT NULL CHECK (cipher_version >= 1),
    key_id TEXT NOT NULL,
    secret_revision INTEGER NOT NULL CHECK (secret_revision >= 1),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK ((plugin_instance_id IS NULL) <> (mcp_setting_id IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS ai_plugin_secret_kind
    ON ai_integration_secrets(plugin_instance_id, secret_kind) WHERE plugin_instance_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS ai_mcp_secret_kind
    ON ai_integration_secrets(mcp_setting_id, secret_kind) WHERE mcp_setting_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS ai_knowledge_entries (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    kind TEXT NOT NULL,
    connection_id INTEGER,
    database_name TEXT,
    namespace TEXT,
    agent_id TEXT REFERENCES ai_agents(id) ON DELETE RESTRICT,
    scope_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(scope_json)),
    content_json TEXT NOT NULL CHECK (json_valid(content_json)),
    source_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(source_json)),
    status TEXT NOT NULL CHECK (status IN ('proposed','confirmed','retired')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ai_knowledge_kind_status ON ai_knowledge_entries(kind, status);
```

字段约束补充：endpoint 禁止 embedded credentials/控制字符，非敏感 Provider JSON 禁止 auth headers；capabilities/config/permissions/tool lists 需限定形状，不因 json_valid 就直接可信。default_decision_model和decision角色必须decision；system绑定reconcile不得覆盖user绑定。ai_agents的默认/主/压缩/产物模型必须generation，decision role必须decision，提示词由Agent直接输入并限制长度；Jev binding 不可 required，这些项目需要应用事务校验，不能被普通保存接口绕过。

### 42.11 运行、产物与计量完整基线 DDL

```sql
CREATE TABLE IF NOT EXISTS ai_mcp_server_sessions (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES ai_mcp_server_profiles(id) ON DELETE RESTRICT,
    client_info_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(client_info_json)),
    protocol_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('connected','disconnected','expired','failed')),
    next_request_seq INTEGER NOT NULL DEFAULT 1 CHECK (next_request_seq >= 1),
    started_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER,
    UNIQUE(profile_id, id)
);
CREATE TABLE IF NOT EXISTS ai_mcp_server_requests (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    protocol_request_id_json TEXT NOT NULL CHECK (json_valid(protocol_request_id_json)),
    tool_id TEXT NOT NULL,
    tool_version INTEGER NOT NULL CHECK (tool_version >= 1),
    resource_grant_id TEXT,
    target_json TEXT NOT NULL CHECK (json_valid(target_json)),
    arguments_redacted_json TEXT NOT NULL CHECK (json_valid(arguments_redacted_json)),
    plan_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'prepared','running','waiting_approval','succeeded','failed','denied','cancelled','unknown')),
    receipt_json TEXT CHECK (receipt_json IS NULL OR json_valid(receipt_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    FOREIGN KEY(profile_id, session_id)
        REFERENCES ai_mcp_server_sessions(profile_id, id) ON DELETE RESTRICT,
    FOREIGN KEY(profile_id, resource_grant_id)
        REFERENCES ai_mcp_server_resource_grants(profile_id, id) ON DELETE RESTRICT,
    UNIQUE(session_id, seq),
    UNIQUE(session_id, id)
);
CREATE INDEX IF NOT EXISTS ai_mcp_server_requests_status
    ON ai_mcp_server_requests(profile_id, status, started_at_ms);

CREATE TABLE IF NOT EXISTS ai_sessions (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES ai_agents(id) ON DELETE RESTRICT,
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active','archived')),
    next_message_seq INTEGER NOT NULL DEFAULT 1 CHECK (next_message_seq >= 1),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ai_sessions_recent ON ai_sessions(status, updated_at_ms DESC);
CREATE TABLE IF NOT EXISTS ai_turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
    status TEXT NOT NULL CHECK (status IN (
        'queued','preparing','generating','executing_tools','waiting_approval','waiting_user',
        'cancel_requested','cancelled','completed','failed','interrupted','outcome_unknown')),
    target_json TEXT NOT NULL CHECK (json_valid(target_json)),
    runtime_snapshot_json TEXT NOT NULL CHECK (json_valid(runtime_snapshot_json)),
    iteration_count INTEGER NOT NULL DEFAULT 0 CHECK (iteration_count >= 0),
    next_event_seq INTEGER NOT NULL DEFAULT 1 CHECK (next_event_seq >= 1),
    error_code TEXT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    UNIQUE(session_id, ordinal),
    UNIQUE(session_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS ai_one_active_turn_per_session ON ai_turns(session_id)
WHERE status IN ('queued','preparing','generating','executing_tools','waiting_approval',
                 'waiting_user','cancel_requested');
CREATE TABLE IF NOT EXISTS ai_messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    turn_id TEXT,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    role TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    persistence_kind TEXT NOT NULL CHECK (persistence_kind IN ('final','checkpoint')),
    redaction_version INTEGER NOT NULL DEFAULT 1 CHECK (redaction_version >= 1),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(session_id, seq),
    UNIQUE(session_id, id),
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS ai_message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES ai_messages(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    kind TEXT NOT NULL CHECK (kind IN (
        'text','reasoning_summary','plan','context_ref','tool_call_ref','tool_result_ref','artifact_ref','notice')),
    status TEXT NOT NULL CHECK (status IN ('pending','streaming','complete','interrupted','error')),
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version >= 1),
    payload_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload_json)),
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    result_id TEXT REFERENCES ai_results(id) ON DELETE RESTRICT,
    artifact_id TEXT REFERENCES ai_artifacts(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(message_id, ordinal),
    CHECK (kind NOT IN ('tool_call_ref','tool_result_ref') OR tool_call_id IS NOT NULL),
    CHECK (kind <> 'artifact_ref' OR artifact_id IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS ai_parts_message_order ON ai_message_parts(message_id, ordinal);
CREATE TABLE IF NOT EXISTS ai_session_ui_state (
    session_id TEXT PRIMARY KEY REFERENCES ai_sessions(id) ON DELETE RESTRICT,
    selected_turn_id TEXT,
    expanded_part_ids_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(expanded_part_ids_json)),
    scroll_anchor_message_id TEXT,
    ui_options_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(ui_options_json)),
    draft_json TEXT CHECK (draft_json IS NULL OR json_valid(draft_json)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, selected_turn_id)
        REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    FOREIGN KEY(session_id, scroll_anchor_message_id)
        REFERENCES ai_messages(session_id, id) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS ai_tool_calls (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    iteration INTEGER NOT NULL CHECK (iteration >= 1),
    source_ordinal INTEGER NOT NULL CHECK (source_ordinal >= 0),
    provider_call_id TEXT NOT NULL,
    tool_id TEXT NOT NULL,
    tool_version INTEGER NOT NULL CHECK (tool_version >= 1),
    plugin_digest TEXT,
    arguments_redacted_json TEXT NOT NULL CHECK (json_valid(arguments_redacted_json)),
    target_json TEXT NOT NULL CHECK (json_valid(target_json)),
    plan_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'prepared','waiting_approval','running','succeeded','failed','denied','cancelled','unknown')),
    receipt_json TEXT CHECK (receipt_json IS NULL OR json_valid(receipt_json)),
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(turn_id, iteration, provider_call_id),
    UNIQUE(turn_id, iteration, source_ordinal)
);
CREATE INDEX IF NOT EXISTS ai_calls_turn_status ON ai_tool_calls(turn_id, status);
CREATE TABLE IF NOT EXISTS ai_approvals (
    id TEXT PRIMARY KEY,
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    mcp_request_id TEXT REFERENCES ai_mcp_server_requests(id) ON DELETE RESTRICT,
    plan_hash TEXT NOT NULL,
    scope_json TEXT NOT NULL CHECK (json_valid(scope_json)),
    status TEXT NOT NULL CHECK (status IN ('pending','approved','denied','expired','invalidated')),
    reason_code TEXT,
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER,
    expires_at_ms INTEGER,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    CHECK ((tool_call_id IS NULL) <> (mcp_request_id IS NULL)),
    UNIQUE(tool_call_id, plan_hash),
    UNIQUE(mcp_request_id, plan_hash)
);
CREATE TABLE IF NOT EXISTS ai_tool_audits (
    id TEXT PRIMARY KEY,
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    mcp_request_id TEXT REFERENCES ai_mcp_server_requests(id) ON DELETE RESTRICT,
    stage TEXT NOT NULL CHECK (stage IN ('pre_execute','revalidate')),
    hook_id TEXT NOT NULL,
    hook_version TEXT NOT NULL,
    ruleset_version TEXT NOT NULL,
    plan_hash TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('allow','deny','require_approval')),
    risk TEXT NOT NULL,
    rule_ids_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(rule_ids_json)),
    reason_codes_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(reason_codes_json)),
    created_at_ms INTEGER NOT NULL,
    CHECK ((tool_call_id IS NULL) <> (mcp_request_id IS NULL))
);
CREATE INDEX IF NOT EXISTS ai_audits_tool_time ON ai_tool_audits(tool_call_id, created_at_ms);
CREATE INDEX IF NOT EXISTS ai_audits_mcp_time ON ai_tool_audits(mcp_request_id, created_at_ms);

CREATE TABLE IF NOT EXISTS ai_run_events (
    turn_id TEXT NOT NULL REFERENCES ai_turns(id) ON DELETE RESTRICT,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    kind TEXT NOT NULL,
    summary_json TEXT NOT NULL CHECK (json_valid(summary_json)),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(turn_id, seq)
);

CREATE TABLE IF NOT EXISTS ai_results (
    id TEXT PRIMARY KEY,
    session_id TEXT,
    turn_id TEXT,
    source_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    mcp_session_id TEXT REFERENCES ai_mcp_server_sessions(id) ON DELETE RESTRICT,
    mcp_request_id TEXT,
    metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
    retention_kind TEXT NOT NULL CHECK (retention_kind IN ('memory','retained')),
    status TEXT NOT NULL CHECK (status IN ('available','expired','deleted','failed')),
    relative_path TEXT,
    byte_count INTEGER NOT NULL DEFAULT 0 CHECK (byte_count >= 0),
    content_hash TEXT,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    FOREIGN KEY(mcp_session_id, mcp_request_id)
        REFERENCES ai_mcp_server_requests(session_id, id) ON DELETE RESTRICT,
    CHECK (
        (session_id IS NOT NULL AND turn_id IS NOT NULL
         AND mcp_session_id IS NULL AND mcp_request_id IS NULL)
        OR
        (session_id IS NULL AND turn_id IS NULL AND source_call_id IS NULL
         AND mcp_session_id IS NOT NULL AND mcp_request_id IS NOT NULL)
    ),
    CHECK (retention_kind <> 'retained' OR relative_path IS NOT NULL)
);
CREATE TABLE IF NOT EXISTS ai_artifacts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    title TEXT NOT NULL,
    renderer_id TEXT NOT NULL,
    renderer_version TEXT NOT NULL,
    spec_json TEXT NOT NULL CHECK (json_valid(spec_json)),
    status TEXT NOT NULL CHECK (status IN ('draft','staged','ready','failed','expired')),
    relative_path TEXT,
    byte_count INTEGER NOT NULL DEFAULT 0 CHECK (byte_count >= 0),
    content_hash TEXT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS ai_artifact_sources (
    artifact_id TEXT NOT NULL REFERENCES ai_artifacts(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    result_id TEXT REFERENCES ai_results(id) ON DELETE RESTRICT,
    tool_call_id TEXT REFERENCES ai_tool_calls(id) ON DELETE RESTRICT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    PRIMARY KEY(artifact_id, ordinal),
    CHECK (result_id IS NOT NULL OR tool_call_id IS NOT NULL)
);
CREATE TABLE IF NOT EXISTS ai_context_checkpoints (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 1),
    first_message_seq INTEGER NOT NULL CHECK (first_message_seq >= 1),
    last_message_seq INTEGER NOT NULL CHECK (last_message_seq >= first_message_seq),
    summary_text TEXT NOT NULL,
    protected_items_json TEXT NOT NULL CHECK (json_valid(protected_items_json)),
    source_hash TEXT NOT NULL,
    model_profile_id TEXT REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(turn_id, generation)
);
CREATE TABLE IF NOT EXISTS ai_usage_ledger (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    model_profile_id TEXT NOT NULL REFERENCES ai_model_profiles(id) ON DELETE RESTRICT,
    budget_policy_id TEXT REFERENCES ai_budget_policies(id) ON DELETE RESTRICT,
    purpose TEXT NOT NULL CHECK (purpose IN ('main','decision','compaction','artifact')),
    input_tokens INTEGER CHECK (input_tokens >= 0),
    output_tokens INTEGER CHECK (output_tokens >= 0),
    cached_input_tokens INTEGER CHECK (cached_input_tokens >= 0),
    cost_micros INTEGER CHECK (cost_micros >= 0),
    measurement TEXT NOT NULL CHECK (measurement IN ('actual','estimated','unknown')),
    pricing_snapshot_json TEXT NOT NULL CHECK (json_valid(pricing_snapshot_json)),
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(request_id, attempt)
);
CREATE INDEX IF NOT EXISTS ai_usage_budget_time ON ai_usage_ledger(budget_policy_id, created_at_ms);
CREATE INDEX IF NOT EXISTS ai_usage_session ON ai_usage_ledger(session_id, created_at_ms);
CREATE TABLE IF NOT EXISTS ai_budget_reservations (
    id TEXT PRIMARY KEY,
    request_id TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    budget_policy_id TEXT REFERENCES ai_budget_policies(id) ON DELETE RESTRICT,
    amount_micros INTEGER NOT NULL CHECK (amount_micros >= 0),
    status TEXT NOT NULL CHECK (status IN ('held','settled','released','unknown')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(session_id, turn_id) REFERENCES ai_turns(session_id, id) ON DELETE RESTRICT,
    UNIQUE(request_id, attempt)
);
CREATE INDEX IF NOT EXISTS ai_reservations_budget_status
    ON ai_budget_reservations(budget_policy_id, status, created_at_ms);
```

### 42.12 应用层补充校验与删除策略

- MCP resource_grant 必须属于 request 的 Profile；clientInfo 不授权；protocol_request_id 允许后续复用，仅 server id/seq 作为记录身份。
- result/approval 的内部和 MCP owner 必须二选一；MCP result 的 session/request 组合 FK 防跨 Client 取数据。
- parts的tool/result/artifact、UI scroll anchor和selected turn必须属于本session或明确授权来源；审计绑定最终plan hash，不能把旧判断挪到新参数。
- session/turn 的 source_call、artifact source、context seq、usage session 必须实际归属于同一任务；本 DDL 的一般 FK 不自动证明所有跨行语义，storage 事务显式校验。
- Activity index 不包含 interrupted/unknown；允许用户开启新 Turn，但若同资源有未知写操作，权限服务阻止冲突写入，不能因为 index 允许就继续危险操作。
- 清理已归档会话按依赖顺序显式确认并删除；不依靠关闭 FK 来清数据。秘密 Clear 不是删除模型行。
- 运行记录表不允许 plugin 直接写入；receipt/event/usage 由内核保存。render/spec 参数不能伪装主模型的工具成功事件。
- secret/arguments/spec/message 的大小与敏感数据策略由应用层执行；数据库的 json_valid 只验证语法。

### 42.13 具体存储 API 与测试落点

拟定 repository 方法：`list_models_page/get_model/save_model/delete_model`、`save_agent`、`save_agent_system_prompt`、`save_plugin_instance/save_binding`、`save_mcp_settings`、`save_budget_policy/save_runtime_settings`；每个可编辑对象带 expected_revision。

append 路径：`create_session/start_turn/append_message_header/append_message_part/checkpoint_stream_part/save_session_ui_state/save_panel_preferences/prepare_tool_call/append_tool_audit/resolve_approval/finish_tool_call/append_event/commit_context_checkpoint/register_result/save_artifact/reserve_budget/settle_usage`；有多个表变化时用同一个事务。

测试放 [storage ai tests](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/ai_platform/tests)，按 schema/init/models/secrets/cas/messages/parts/ui_state/audits/runtime 拆分；app 测试用可替换 GenerationProvider、DecisionProvider、AiTool 和仓库能力，不直接触碰真实用户配置。

本文件的 SQL 必须在 SQLite 启用 FK 后实际建表并运行代表性约束/CAS/trigger 测试。实现阶段再通过 Rust bundled rusqlite 做同样测试，以排除系统 SQLite 与 bundled 版本能力差异。

### 42.14 本轮文档验证记录

2026-10-03 在本机执行了文档/内存 SQLite 验证（SQLite 3.51.2）：初版两个基线 DDL 块成功创建 **27 张独立 AI 表**，全部表有代表性插入，foreign_key_check 无错误。初版共 **31 项检查通过**，覆盖必需九类实体、重复建表、密文列组合/nonce 形态、Keep/Clear、对象 CAS、当时的Agent内提示词规则（现已移除）、单 Session 活动 Turn、跨 Session FK、JSON、引用删除、usage 唯一性、任务引用、源码路径与截图约束。

初版检查了 25 个唯一的已存在本地文件链接；9 个不存在的路径均已标为拟新增。初版详细实施任务为 53 项，增补 MCP 后为 63 项，全部未宣布实现完成。外部资料入口记录在 §5，不把远程主分支功能当成已验证的 SDK 发布版。

**未执行** Rust 实现/编译、AES 实际加解密、真实模型调用、GUI 截图或当前开发平台 E2E；它们是任务清单中的实施验收，不能从本轮 DDL 检查推导为已经通过。测试用 BLOB 只验证列约束，不是生产密文。

### 42.15 对外 MCP Server 增补验证范围

本次将 MCP Server 与 Client 分开，新增四表，该次基线总数为31；本轮新增parts/UI/audit并删除不必要迁移表，当前33表。results/approvals 增加互斥 owner；本次实际内存SQLite验证为31表/44项通过，36个本地链接有效，16个路径标注为拟新增；本章历史验证记录注明范围。这是结构/约束验证，不是MCP功能已实现。CLI 检查仅 `claude --version/--help/mcp --help/mcp add --help/mcp list --help`，没有执行 add/list 或真实模型调用。完整 MCP/Claude/UI 验收由新增 T054–T063 承担。

### 42.16 本轮范围修订

不实施旧AI迁移/第三方加载；首次建表33张（原31减migration表，加message parts/UI state/tool audits三表）。普通message正文只在parts，避免双份content_json；思考仅公开内容/摘要，工具状态从事实表重建。默认决策模型与system/user绑定来源列支持内置Jev自动启用。

具体初始DDL/约束验证与当前任务数在本轮验证记录更新；这些仍是文档/内存SQLite检查，不是Docker、GUI、模型或第三方插件已经执行。

**就近参考：**[现有SQLite职责](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/sqlite.rs)；[Goose表/消息实例](/Users/shining3d/fusuwei/code/goose/crates/goose/src/session/session_manager.rs)。实际落点按本章/§7，不照搬源项目私有状态。


### 42.17 当前修订验证记录

2026-10-03，SQLite3.51.2内存验证：**33表、65项文档/DDL/约束检查通过**。新增message parts单一正文、part ordinal/FK/CAS、UI跨session锚点与Turn、公开摘要/折叠状态、audit owner互斥、默认决策模型FK与system来源；确认无ai_schema_migrations。任务66项中58项本期、8项延期，本期不依赖延期项且依赖图无环；35个实现章节有就近参考，52个现存本地链接有效、16个路径标注拟新增。

本轮未执行Docker连接/SQL写入、GUI截图、AES实际加解密、主模型/Jev或Claude MCP任务。已配置Docker的可测试性是用户确认；数据来源与测试namespace执行时再核对。以上结构验证不能代替真实E2E或自动reconcile算法验收。

### 42.18 设计内容合并记录

工具/MCP和内置插件/设置/UI/持久化的完整细节已按职责并入本文件§18、§20–23、§25–28、§31、§35–37、§39等对应章节；顶部阅读索引直接跳到主文锚点。任务清单改为引用这些主文章节。旧专项文件只保留短跳转说明，不是第二份设计来源。

合并没有更改基线DDL（两个SQL块逐字节一致）、任务范围或依赖。重新运行内存SQLite/文档检查：33表、70项通过，50个现存本地链接与主文内部锚点有效；66任务仍为58项本期/8项延期且依赖无环。未执行功能代码、Docker、GUI或真实模型测试。

### 42.19 两种询问AI形式与打开规则增补

§31.4–31.6统一定义首次抽屉/之后记忆、60%/40%上限、320dp正常最小/窄窗口有效最小、面板内切换和上次会话恢复。新增ai_panel_preferences独立表，基线34表；其他DDL与任务范围保持，仅扩充现有T015/T064/T066验收。本次文档/内存SQLite验证另记，不代表两种UI或真实会话恢复已实现。

### 42.20 Agent直接提示词与配置字段修订（再简化）

按用户最新需求删除独立提示词模板/多版本/发布及两张相关表；提示词直接保存在ai_agents.system_prompt；随后又删除default_target_mode/连接/库/namespace四列（目标改为§13自动捕获）与用户费用上限。Agent revision仅为并发控制，Turn快照保留当次正文/hash，不提供提示词历史管理。当前32表。§31.8是Agent编辑参数的实际设计依据，参考UI分区但不照搬Jev配置；询问AI字体缩小，其他管理表单不全局缩小。

### 42.21 模型字段与Key显示修订

按用户要求：大模型配置新增上下文窗口Token、最大输出Token、计价（输入/输出/缓存读取），三者在§42.10建为独立列；缺失价格只影响用量展示准确性，不当免费。API Key输入框默认掩码`***`，末尾眼睛按钮为用户主动显式解密查看的例外路径（短时内存、脱敏审计、关闭即掩码）；默认操作仍是Keep/Set/Clear，密文永远在ai_model_profiles行。

### 42.21 已落地代码（进行中）

| 任务 | 文件 | 状态 |
|---|---|---|
| T006 首次建表 | `crates/fluxdb-storage/src/parts/ai_platform/{mod,schema}.rs` | 32表幂等创建 + 结构校验，接入 `sqlite::create_schema` |
| T007 共享密钥 | `crates/fluxdb-storage/src/parts/secret_crypto.rs` | `secret_key` 优先、兼容 `connection_secret_key`、冲突拒绝；连接凭据已改用 |
| T008 模型密文 | `crates/fluxdb-storage/src/parts/ai_platform/model_secret.rs` | `SecretInput`(不可序列化/Debug脱敏)、AAD绑定模型行与代次、状态枚举 |
| T009 模型保存 | `crates/fluxdb-storage/src/parts/ai_platform/models.rs` | Keep/Set/Clear + `WHERE revision=?` CAS，公开视图不含密文 |
| T015 会话持久化 | `crates/fluxdb-storage/src/parts/ai_platform/sessions.rs` | 会话/Turn/消息/片段/运行事件/会话UI状态；seq·ordinal原子分配、单活跃Turn、流式片段合并不插行、终态Turn拒绝迟到事件、快照恢复；`reasoning_summary` 拒绝私有 CoT 字段 |
| T002 核心 DTO/契约 | `crates/fluxdb-core/src/parts/ai/mod.rs` | `CancelFlag`、`ModelCapabilities`、`ModelRequest`/`ModelEvent`/`Usage`/`FinishReason`、`ToolDescriptor`/`ToolCallRequest`/`ToolReceipt`、`DecisionRequest`/`DecisionResponse`；`GenerationProvider`/`DecisionProvider`/`AiTool` trait（`dyn` 多实现）。凭据类型仍由 storage `model_secret::SecretInput` 单点提供（已满足不可序列化 + Debug 脱敏），不在 core 造第二份 |
| T003 流式传输 | `crates/fluxdb-app/src/parts/ai_transport.rs` | 增量 SSE 解析（`data:` 帧、CRLF、`[DONE]`）、OpenAI/Anthropic 分片→`ModelEvent`（工具参数跨分片累积、usage、finish_reason）、单帧/整包字节上限、`CancelFlag` 取消、connect/read deadline；`TransportAuth` Debug 脱敏。**不采用 Rig**：它不在 `Cargo.lock`（离线环境需新增大依赖树），且其 loop 不满足"工具执行前必须交还宿主做审批/回执"的契约；模型 HTTP/SSE 复用已锁定的 ureq（同步，调用方放后台线程） |
| T021 界面上下文捕获/目标固定 | `crates/fluxdb-app/src/parts/ai_context.rs` | 宿主用小 DTO（`CapturedTab`，**不含连接敏感配置**）：SQL 编辑器（选区优先 → 否则整段）、表数据、Redis Key、设计页、无标签页（侧栏选中连接）；`capture()` 产出 `UiContextSnapshot`；`bound_excerpt` 按 **16 KiB / 400 行**限额做**头 60% + 尾 40%** 截断并插入 `… [省略 N 行，可用 editor.read_range 读取] …`（字节超限按 UTF-8 边界裁剪）；`pin_target` 固定 `TargetRef`（切标签页不改），`retarget` 仅在用户显式换目标时调用并给出 `invalidated_target_json`（target 变 → plan_hash 变 → 旧审批失效）；无目标时给一次性 `notes`，任务仍可进行 |
| T020 审批/审计/授权策略 | `crates/fluxdb-storage/src/parts/ai_platform/audits.rs` + `crates/fluxdb-app/src/parts/ai_permission.rs` | 存储：`ai_tool_audits` **append-only**（只有插入/查询，无 update/delete；绑定恰好一个 owner；plan_hash 必填）、`ai_approvals` 与 `plan_hash` 绑定（pending→approved/denied 走行级 CAS，`invalidate` 撤销即时生效，`expires_at_ms` 过期，`find_valid_approval` 要求 approved + 未过期 + **hash 完全一致**）。策略：`plan_hash` 覆盖 工具/版本/目标/规范化参数/插件摘要（任一变化即失效）、`KernelPolicy` 与风险求交集、`authorize` **Deny > RequireApproval > Allow**（Agent 白名单先收窄、写操作一律需审批、管理类默认拒绝、审计 Deny 直接拒绝）、`audit_or_fail_closed` 审计不可用即拒绝 |
| T019 工具目录/schema/结果协议 | `crates/fluxdb-app/src/parts/ai_tool_registry.rs` | `ToolSpec`（含 input/output schema、risk、effect、timeout、output_limit）；`ToolCatalog::{discover, describe, prepare, register, executable}` —— **发现只给摘要、不带 schema、不授予执行权限**，`describe` 才取 schema，`prepare` 先做 schema 校验（未知工具/体积超限/校验失败都是受控 `ToolError{code,message,retryable}`，不做类型强转与"猜参数"）；`validate_schema` 支持 `$ref(#/$defs)`/type/required/properties/additionalProperties:false/enum/minLength·maxLength/minimum·maximum/minItems·maxItems/items 子集；`ResultStore`+`ResultRef`+`ResultPage`（行数/字节双上限、`truncated`、`next_cursor`、释放后 `result_expired`、过期 `unknown_result`）；`project_receipt` 生成**有界模型投影**（UTF-8 边界安全截断）且**不修改原始回执** |
| T018 主循环/工具状态机 | `crates/fluxdb-app/src/parts/ai_loop.rs` | `ToolRegistry`（宿主 ID→工具实现）；`TurnRunner::run` 同步主循环：吸收 steering → stream → 组装（文本/工具调用/结束原因）→ **截断(`Length`)不执行工具**（参数可能不完整，结构化说明回给模型）→ 非法 JSON 不执行（不猜参数）→ 未注册工具拒绝 → 顺序执行工具取**真实回执** → 回执（含 Failed）作为 Tool 消息进入下一轮 → 无工具调用即 `Completed`；相同 (工具,参数) 重复 ≥ 阈值 → `Paused`（无进展）；工具 `Err`（基础设施）才 `Failed`；不依赖 DecisionProvider（无 Jev 可完整运行） |
| T017 主模型 Provider 适配 | `crates/fluxdb-app/src/parts/ai_provider.rs` | `CredentialLease`/`SecretLease`（**请求前**才租明文，不可 Clone/Serialize/Display，Debug `[REDACTED]`，只在构造认证头的作用域内存在）；`ToolNameMapper`（宿主 `db.query` ↔ Provider `db_query` 可逆、注册冲突报错、回程只认宿主 ID）；`build_chat_body`（**不含凭据**；仅 `tool_calling` 为真才带 tools；OpenAI/Anthropic 两种形态）；`ChatTransport` 抽象（生产 `UreqChatTransport`，测试注入假实现）；`HttpChatProvider` 能力门控 + 未注册工具/能力不匹配即报错。传输侧 `stream_chat` 显式 `max_redirects(0)`（不跟随重定向，避免跨域把认证头引向非预期主机） |
| T016 AI 运行时 actor | `crates/fluxdb-app/src/parts/ai_runtime.rs`、`ai_conversation_runtime.rs`、`ai_conversation_delivery.rs` | 统一串行 actor 管理 Provider 执行与持久化会话任务；桌面发送/停止/审批恢复提交到有界队列，不自行创建执行线程。活跃/排队取消走旁路取消位，steering 按 Turn 隔离并在每次模型请求前消费；控制锁不覆盖外部调用。会话更新队列与独立终态槽避免满队列阻塞收尾。析构只请求关闭并后台等待；显式 `shutdown(timeout)` 超时保留句柄。应用退出回调请求取消，但同步外部调用可能超过 GPUI 的 200ms 退出窗口，不能宣称均已安全结束，未知副作用仍按恢复协议处理。2026-10-08 自动化验证通过，真实窗口受阻，T016 未勾选；详见 `docs/testing/ai-agent/t016-runtime-unification-2026-10-08.md`。 |
| T004 scripted Provider | `crates/fluxdb-app/src/parts/ai_scripted.rs` | `ScriptedGenerationProvider`/`ScriptedDecisionProvider`/`RecordingTool`：脚本只决定模型输出，工具必须真实执行且回执回到下一轮请求；不读生产配置、不发网络、不花费用 |

后续任务按 `docs/ai-agent-tasks.md` 顺序推进。

### 42.22 实现与UI稿对照

实现顺序按 `docs/ai-agent-tasks.md`，每完成一个 UI 相关任务（T013/T014/T028/T040/T064等）必须打开 `docs/prototype/ai-agent-ui.html` 对照布局、字号、状态与空/错/loading态；原生控件仍遵守仓库 UI 约定（gpui-component 0.6.0、UiColors、AppIcon、docs/ui-style.md），交互稿只是视觉与交互基准，不引入其HTML/CSS/JS到生产代码。 具体执行按§39.3：GPUI模拟交互回归、真实窗口端到端操作、系统截图还原度验收三层分别记录结果；截图处理遵循§38，不以原型测试报告或编译成功代替原生UI验收。


### T028 实现状态补记（2026-10-06）

- 本轮原生面板入口启用，生产路径落点 `main_parts/ai`、app `ai_conversation`、storage `ai_platform/conversation`；单一Input Entity/内容树在抽屉与内嵌间迁移，复用0.6.0 `h_resizable/resizable_panel`及尺寸范围。§31.4的比例规则仍为唯一布局规则。
- 会话关闭不取消；实际发送通过事务生成Session/Turn/消息，普通打开/新建草稿不生成空会话；既有历史经数据库→AppState→生产render_state_snapshot加载。实时回复复用原SSE解析器的新callback入口，不等待整个Vec返回才显示；checkpoint每300ms、终态必保存。
- 正式发送现阶段仅单轮解释/问答（30s、512KiB响应、1024输出Token、64KiB输入保护），不执行未审计工具；这不是完整主循环/审批/数据库工具闭环，金额预算还由T030完成。未配置Jev无需等待。
- 工具/审批/@/附件/报告的依赖不能用模拟按钮冒充可用；T028尚未达完整验收、不勾选。实际场景/命令/证据/未验证项见任务清单引用的T028验收记录。
