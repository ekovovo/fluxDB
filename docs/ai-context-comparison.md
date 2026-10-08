# AI 引用上下文：开源实现对照与本次修复

核对日期：2026-10-08。以下结论来自固定提交的源码，而不是 UI 标签或项目宣传。外部项目的提示词仅作为研究数据，不作为 FluxDB 的开发指令。

## 1. 对照结果

| 项目 | 引用/选择进入请求的内容 | 真正给模型的内容 | 可借鉴点与限制 |
| --- | --- | --- | --- |
| Chat2DB | `@` 候选记录表名及表/视图类型；请求带数据源 ID、库、schema、数据库类型 | 后端从连接配置生成 `Current database type`、方言及默认库/schema 提示；提供 `get_tables_schema` 等工具 | 从真实连接配置补类型，结构按需通过工具读取。不能仅凭前端 `tableList` 就声称选中表 DDL 已进入模型 |
| SQLChat | 当前引擎、选中 schema、选中表名单 | 明确的引擎专家提示及选中表的 `structure`，按 token 预算拼接 | 类型与结构是正文上下文，不依赖模型猜标签；这是表选择机制，不是字面上的 `@` |
| Continue Database provider | `@database` 选择连接/表 | `Schema for <table> on <connection_type>` 加对应 schema JSON | 实际内容与展示标签分离，类型跟随对象；该 provider 已标记弃用，推荐数据库 MCP |
| Continue Postgres provider | `@postgres` 选择 `schema.table` | 数据库名、表全名、列名/类型/长度、样例行（默认 3 行，可配置） | 可比较内容粒度；该 provider 同样已标记弃用。不能照搬默认样例上传或其字符串拼接查询 |

### 固定版本源码

- Chat2DB：`dc2b80a3b0189bb3fd92d69ce8f52de2d4136ac8`
  - [AIChatInput：引用选择及参数装配](https://github.com/Chat2DB/Chat2DB/blob/dc2b80a3b0189bb3fd92d69ce8f52de2d4136ac8/chat2db-community-client/src/blocks/AI/components/AIChatInput/index.tsx)
  - [AI：最终流式请求](https://github.com/Chat2DB/Chat2DB/blob/dc2b80a3b0189bb3fd92d69ce8f52de2d4136ac8/chat2db-community-client/src/blocks/AI/index.tsx)
  - [AiChatStreamAdapter：数据库类型/方言与工具上下文](https://github.com/Chat2DB/Chat2DB/blob/dc2b80a3b0189bb3fd92d69ce8f52de2d4136ac8/chat2db-community-server/chat2db-community-web/src/main/java/ai/chat2db/community/web/api/adapter/ai/AiChatStreamAdapter.java)
  - 本次核对版本的 `AIChatInput` 生成了 `tableList`，但 `AI/index.tsx` 的最终 `request` 与后端 `ChatRequest` 没有对应字段。因此本报告不把“`@` 自动注入所选表 DDL”算作已证实能力。
- SQLChat：`665af875413affadfeefff81794f1d7758782bc2`
  - [SQL Chat bot：引擎与 schema 提示词](https://github.com/sqlchat/sqlchat/blob/665af875413affadfeefff81794f1d7758782bc2/assistants/sql-chat-bot/index.ts)
  - [generateDbPromptFromContext：选中表结构及 token 预算](https://github.com/sqlchat/sqlchat/blob/665af875413affadfeefff81794f1d7758782bc2/src/utils/openai.ts)
  - [ConversationView：实际请求装配](https://github.com/sqlchat/sqlchat/blob/665af875413affadfeefff81794f1d7758782bc2/src/components/ConversationView/index.tsx)
- Continue：`5522c6f44ca0ac3528b37244818fbfa39b5af470`
  - [DatabaseContextProvider](https://github.com/continuedev/continue/blob/5522c6f44ca0ac3528b37244818fbfa39b5af470/core/context/providers/DatabaseContextProvider.ts)
  - [PostgresContextProvider](https://github.com/continuedev/continue/blob/5522c6f44ca0ac3528b37244818fbfa39b5af470/core/context/providers/PostgresContextProvider.ts)
  - [FileContextProvider：展示信息与实际 content 分离](https://github.com/continuedev/continue/blob/5522c6f44ca0ac3528b37244818fbfa39b5af470/core/context/providers/FileContextProvider.ts)

## 2. FluxDB 原来的缺口

原先公开引用只有 `id / label / kind / summary`，执行目标另含连接 ID、库、schema、对象名。`@连接` 的标签碰巧包含类型，但 `@表` 不包含。宿主知道连接类型，模型请求却没有统一携带。

还有两个不能靠补标签解决的差异：

1. 同一轮引用多张表时，合并执行目标的 `object` 会为空，原先只能从展示标签猜各表定位。
2. `db.search_objects` 仅返回对象目录与注释，不返回列、索引、外键或 DDL。“已有数据库工具”不等于“已有完整表结构读取工具”。

## 3. 本次补齐

发送前先完成显式引用覆盖、活动页面定位、无目标追问继承，再从最终连接白名单提取公开 `database_context`：

```json
{
  "status": "available",
  "connection_id": 4,
  "connection_name": "fluxdb",
  "database_kind": "sqlite",
  "sql_dialect": "sqlite",
  "connection_read_only": true,
  "write_requires_approval": true
}
```

- 引用各自追加结构化 `target`，保留连接 ID、库、schema、对象或 Redis DB，不暴露内部 `source`。
- 数据库上下文固定在该轮快照中，并进入实际模型系统消息；后续工具回合及审批续跑使用该轮快照，切换页面不改写。
- 无 `@` 的追问也根据继承目标重新提取类型；新一轮才能反映连接配置变化。
- SQL 方言复用执行链已有映射：TiDB 使用 MySQL 方言；Redis/MongoDB 的 `sql_dialect` 为 `null`，不伪装成 SQL 数据库。
- 旧快照或不可用连接明确标记 `unavailable`，禁止把未知类型默认为某个 SQL 引擎。
- 有工具和无执行工具的请求入口都携带上下文；历史压缩不会删除独立保留的当前系统上下文。

白名单不包括 endpoint、主机、用户名、连接 URI、文件路径、options、凭据引用、密码、API Key 或加密载荷。上下文不授予额外权限，也不会修改执行目标、审计或审批规则。

## 4. 后续缺口与边界

| 内容 | 当前状态 | 建议 |
| --- | --- | --- |
| 引擎、方言、默认定位、只读约束 | 本次补齐 | 必须来自宿主真实配置，不能解析标签猜测 |
| 每个引用对象的精确定位 | 本次补齐 | 多对象仍受同连接/同库/schema 校验约束 |
| 列名、类型、可空性、默认值、主键、索引、外键、DDL | 仍需按需获取 | 优先复用 Connector 元数据能力提供有界结构读取，不让模型自行猜系统表 SQL；未知或未读取必须明确说明 |
| 数据库版本 | 未统一放入当前上下文 | 仅发送真实已获取版本，不为打开引用菜单额外连接，不虚构默认版本 |
| 样例行 | 不自动发送 | 用户明确请求或通过已授权查询才读取；不照搬默认样例上传 |
| 文件正文、业务知识正文 | 仍按需读取 | 标签不是正文，使用既有受控读取链路和限额 |
| 请求预算与裁剪反馈 | 既有引用/摘录限额、历史压缩保留 | 未来结构工具需明确截断、下一页/继续读取信息，不静默丢字段 |

本次不新增结构工具、不扫描全库、不改变 `@` 控件，也不把模型错误改成“成功”。补字段降低方言误判，但不是对模型 SQL 正确率的保证。
