# ③ 真实模型 + Docker MySQL/PostgreSQL 端到端：本轮受阻记录（2026-10-06，macOS）

## 结论

**③ 未执行**：两个前置条件在本机当前环境都不满足，因此 T029/T024 等任务保持未勾选。

| 前置 | 期望 | 实际 | 证据 |
|---|---|---|---|
| Docker MySQL | `127.0.0.1:36531`（连接 2 `docker`）可连 | **Connection refused** | `python3 socket.create_connection` 报 `Errno 61` |
| Docker PostgreSQL | `127.0.0.1:5432`（连接 1 `libredesk`）可连 | **Connection refused** | 同上 |
| Docker Redis | `127.0.0.1:6379`（连接 3）可连 | **Connection refused** | 同上 |
| 真实模型「公司自建」 | `anthropic-messages` 可调用 | **nginx 403 Forbidden** | 见下 |

## 模型端点诊断（未携带任何凭据）

- 配置：`公司自建` / `anthropic-messages` / `vllm/DeepSeek-V4-Flash` / `https://ysy.shining3d.com.cn/open-api/maas/model/anthropic`。
- 用应用自身凭据链路跑一次最小真实请求（`crates/fluxdb-app/examples/ai_live_smoke.rs`，`max_output_tokens=16`）：
  `FLUXDB_AI_LIVE=1 FLUXDB_AI_LIVE_MODEL=公司自建 cargo run -p fluxdb-app --example ai_live_smoke`
  → `provider 就绪（凭据已在进程内解密，不外显）`，随后 **`HTTP 403`**（密钥未打印）。
- **不带密钥**探测（排除"是密钥问题"）：
  - `POST https://ysy.shining3d.com.cn/open-api/maas/model/anthropic/v1/messages` → `403` + `nginx/1.24.0` 的 HTML 403 页；
  - `POST .../anthropic/messages` → 同样 `403`；
  - `GET https://ysy.shining3d.com.cn/`（根路径）→ 同样 `403`。
  ⇒ **整站 403**，属网络/IP 白名单或 VPN/内网限制，不是应用或密钥问题。
- 出网本身正常：`https://open.bigmodel.cn/api/v1/models` → `200`，返回 `"Header中未收到Authorization参数"`（说明可达且要求鉴权）。

## 若要继续 ③ 需要

1. **启动 Docker 数据库**（MySQL 36531 / PostgreSQL 5432 / Redis 6379），并给出可用于写入的**测试库/账号**；
   我会自建独立 fixture 表（例如 `fluxdb_ai_smoke`）并在结束时清理，不动你的业务表。
2. **模型可达性二选一**：
   - 在能访问 `ysy.shining3d.com.cn` 的网络/VPN 内运行；或
   - 让 `Zhipu GLM`（`openai-responses`，`https://open.bigmodel.cn` 可达）可用于对话——
     但当前会话链路只实现了 `openai-compatible` 与 `anthropic-messages` 两种 `ChatProtocol`，
     `openai-responses` **只有"测试连接"路径**支持（见 `controller/ai_model_network.rs`）。
     需要先补 `ChatProtocol::OpenAiResponses` 的**请求体 + SSE 解析 + 工具调用映射**才能用它跑端到端。

## 已达成的部分（本轮）

- ① T029 启动恢复扫描/重新审批/未知写防重放：已完成，见 `t029-restart-2026-10-06.md`。
- ② 写工具 `db.execute` 注册 + 固定目标连接权限 + 规范化 plan hash：已完成（真实窗口用内置 demo 连接验证
  "批准 → 复核 → 执行一次 → 回执落库 → UI 结果卡"），见 `t024-write-tool-2026-10-06.md`。
- 新增可复用探针 `crates/fluxdb-app/examples/ai_live_smoke.rs`（必须显式 `FLUXDB_AI_LIVE=1`，只发一次最小请求，绝不打印凭据）。

## 未验证（保持未勾选）

- 真实 MySQL/PostgreSQL 的读取、写入、`affected_rows`、取消与权限拒绝。
- 真实模型的工具选择、多轮迭代、审批交互与失败恢复。
- Windows/Linux。
