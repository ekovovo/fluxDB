# AI 配置管理工具

## 全局工具

`config.inspect` 和 `config.propose` 不依赖设置页或数据库连接。前者只读取非敏感
配置摘要，后者只提交待确认提案；工具回执中的 `proposed` 不表示配置已保存。

| 工具/动作 | 行为 |
| --- | --- |
| `config.inspect` | 读取设置、模型、Agent、全局工具开关、MCP、会话摘要 |
| `setting.update` | 修改白名单内的普通设置 |
| `model.create` / `model.update` | 确认后打开预填好的本地模型表单，用户完成凭据输入并保存 |
| `model.delete` | 删除模型，复用引用检查与对象级 CAS |
| `agent.create` / `agent.update` | 创建或局部修改 Agent，保留未指定字段 |
| `agent.delete` | 复用既有软删除，保留历史会话身份 |
| `tool.set_enabled` | 修改全局开关，与 Agent 白名单取交集，不清空白名单 |
| `session.archive` / `session.purge` | 归档或永久删除会话，永久删除仍要求先归档且无活动任务 |
| `mcp.server_set_enabled` | 启停已登记的本地 MCP Server Profile |
| `mcp.sync_client` | 为 Codex / Claude Code 注册或移除本应用的 MCP 条目；不是安装客户端软件 |

模型/Agent 编辑只传 `changes` 内明确修改的字段和读取到的 `expected_revision`。
模型和 Agent 的内部 ID 应从 `config.inspect` 获取，不能按名称猜测。
运行中的 Agent 不能通过提案编辑或删除自身，仍可在设置页手动操作。

## 确认与保存

1. 用户可在任意页面提出配置需求，AI 先读取摘要，再提交提案。
2. 确认框展示修改内容。删除与客户端配置写入需要额外勾选影响确认。
3. 用户取消不落库；用户确认后，由应用层命令在后台执行。
4. 执行前重新检查版本、过期时间与目标状态。提案有效期为十分钟。
5. 保存成功才更新界面并显示成功；失败保留明确错误，不冒充成功。
6. 已尝试的提案不能再次执行。发生中断时先核实状态，再重新生成提案。

模型表单中的 API Key 只通过既有 Keep/Set/Clear 路径进入加密存储。
配置工具没有 API Key、密码、Token、密文或 nonce 字段。AI 只能看到凭据状态。
模型编辑打开表单只是准备阶段，用户点击该表单的保存按钮后才真正修改模型。

## 页面草稿与工具目录

`page.prefill_form` 仍然是页面绑定的草稿能力，不保存设置。它与全局配置提案分开。
用户已有未保存的草稿时，不允许配置提案覆盖不同的修改。

设置中的工具列表、Agent 的可选工具和 `config.inspect(kind="tools")` 共用应用层
`ai_tool_management.rs` 的管理目录。目录包含条件装配工具，不代表这些工具在当前
任务都有授权。全局关闭仍由执行边界检查，发现工具不等于获得权限。

内部 SQL 执行使用 `db.run`；旧 SQL ID 在界面中标注为兼容入口。
Redis 工具与长尾工具发现尚未在本轮收敛，不应把它们描述为已经完成的能力。
