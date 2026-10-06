# B：`openai-responses` 对话协议 + 真实模型工具链路验收（2026-10-06，macOS）

## 为什么做这个

③ 受阻时发现：唯一可达的真实模型是 `Zhipu GLM`（`openai.bigmodel.cn` 返回 200；`公司自建` 整站 nginx 403），
但它用 `api_protocol=openai-responses`，而对话链路此前只实现 `openai-compatible` / `anthropic-messages`
（`openai-responses` 只有"测试连接"路径）。本任务把它补上，让用户立刻拥有可用的真实模型。

## 实现

**transport（`ai_transport.rs`）**
- `ChatProtocol::OpenAiResponses` + `parse_responses_chunk`：把 `response.*` SSE 事件投影成 `ModelEvent`：
  - `response.output_text.delta` → `TextDelta`；`response.reasoning_summary_text.delta` → `ReasoningSummaryDelta`；
  - `response.output_item.added` + `response.function_call_arguments.delta` + `response.output_item.done` → `ToolCallRequested`
    （按 output item id 累积，done 时以事件里的最终 `arguments` 为准）；
  - `response.completed` → `Usage` + `Finished(Stop)`；`response.incomplete` → `Finished(Length)`；`response.failed` / `error` → `Finished(Error)`。
  - **刻意忽略 `response.reasoning_text.*`（私有推理）**：不得投影成事件，更不得落库。

**provider（`ai_provider.rs` / `ai_conversation_provider.rs`）**
- `build_chat_body` 增加 Responses 分支：system → 顶层 `instructions`；user/assistant → `message` 项；
  assistant 的 `tool_calls` → `function_call` 项；工具结果 → `function_call_output` 项（带 `call_id`）；
  tools → `{type:"function", name, description, parameters}`（名称用 Provider 安全名）。
- `AiConversationProvider::from_storage` 接受 `openai-responses`；认证用 `Authorization: Bearer`。
- **补回程映射（真实模型必需）**：以前只映射去程工具名，模型返回的 `db_query` 会被当成未注册工具而拒绝；
  现在与 `HttpChatProvider` 一致地把 Provider 名映射回稳定 ID，未声明能力却返回工具调用按协议错误处理。

**core / 主循环**
- `ModelMessage` 增加 `tool_call_id` / `tool_name` / `tool_calls`（+ `ModelToolCall` 与 `tool_result` / `assistant_with_tool_calls` 构造器）；
  旧构造 `ModelMessage::new` 行为不变。
- 主循环：assistant 回合记住本回合的工具调用（无状态协议必须重放 `function_call` 才能与 `function_call_output` 配对）；
  工具结果（含受控错误、拒绝、截断）全部带配对 `call_id`，不再出现"没有配对的结果"。

## 真实模型验证（Zhipu GLM，走宿主凭据链路在进程内解密，未打印密钥）

1. **纯文本**：`FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_MODEL='Zhipu GLM'` →
   `finish=Some(Stop) usage=Some(Usage{input:18, output:68, cached:0}) text="就绪"`。
2. **真实工具回合**（合成内存 demo 连接，**不写用户库**）：`FLUXDB_AI_LIVE_TOOLS=1` →
   - 模型自主调用工具：`start:db.query` → 失败，`start:db.search_objects` → 成功，`start:db.query` → 成功（自我纠正后完成）；
   - 最终回答准确复述 demo 数据（2 行：`Road Bike` / `Helmet`，列 `id`/`name`）；
   - 证明：真实模型 → SSE 解析 → 工具名映射 → schema 校验 → 审计 → 授权 → 执行 → 回执 → 回灌模型 → 多轮迭代 全链路可用。

### 真实调用中发现并记录的行为

- Zhipu 会先流大量 `response.reasoning_text.delta`（私有推理）。用 `max_output_tokens=16` 时**思考就把预算吃光**，
  结果只有 `response.incomplete`、没有任何正文。探针现在默认 256 并支持 `FLUXDB_AI_LIVE_MAX_TOKENS` 覆盖。
  这也解释了"上限太小时看起来像空回复"的成因，而不是协议解析失败。
- 该端点不使用标准 `response.reasoning_summary_text.delta`（当前没有公开摘要），因此 UI 不会出现"公开思考摘要"——
  这是模型侧能力问题，不是我们吞掉事件。

## 自动化回归

| 命令 | 结果 |
|---|---|
| 新增 `ai_transport_responses_projects_text_tool_and_completion` / `ai_transport_responses_maps_incomplete_and_failed_without_leaking_reasoning` | 通过（含"私有推理不得投影"断言） |
| 新增 `ai_provider_responses_body_pairs_tool_calls_and_outputs` | 通过（instructions/工具安全名/function_call 与 function_call_output 配对） |
| `cargo test -p fluxdb-core / -p fluxdb-storage / -p fluxdb-app / -p fluxdb-desktop --offline` | 106 / 121+16 / 696(1 ignored)+集成 10·2·6·2·6·4·11 / 505(4 ignored) |
| `cargo check --workspace --offline` | 通过 |

## 未验证 / 未完成

- **真实 MySQL/PostgreSQL 未验收**：三个 Docker 端口仍 Connection refused（见 `t024-live-attempt-2026-10-06.md`），
  所以"真实库写入 + affected_rows + 取消"仍未验证。
- **真实窗口里的真实模型未验收**：本轮真实调用在探针里完成；桌面端要吃真实模型仍需要人工点「发送」并让模型选工具，
  该流程留待 ③ 恢复后执行。
- 只有 `Zhipu GLM` 走通了 Responses；其他 Responses 兼容实现（事件命名差异）未逐一验证。
- 平台：仅 macOS；Windows/Linux 未验证。
