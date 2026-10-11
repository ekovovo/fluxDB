# T035 摘要模型与原子 checkpoint 真实窗口验证记录（2026-10-11，macOS arm64）

## 结论

- **真实主窗口 + 隔离库验证通过 4 组**：① 独立摘要模型被真实使用且压缩计量记在摘要档案；② 压缩状态在会话里可见（成功/失败文案）；③ 摘要模型不可用 → 可见说明 + 超预算可见暂停，且不静默回退主模型；④ 截断摘要不覆盖已有 checkpoint。
- 验收过程中发现并已修复 2 处由本轮引入的观感问题（见"发现与修复"）。
- **未覆盖**：真实第三方模型（公司自建/决策模型）下的压缩表现与真实 token 计量；"正在压缩"pending 态截图（本地 mock 太快，且需人为延时）；原型逐项对照的明暗/窄窗截图由用户执行，只保留了暗色两组（见证据）。

## 环境

- 隔离夹具：`/private/tmp/fluxdb-t035-e2e/env`（配置库 `env/config`，**不含任何真实凭据/连接**），由 `crates/fluxdb-app/examples/ai_t035_fixture.rs` 生成：
  - 主模型 `T035 主模型`（档案 `mock-main`）：窗口 3072 / 最大输出 512 / 占位 Key（mock 不校验）
  - 摘要模型 `T035 摘要模型`（档案 `mock-summary`）：同参数；以 `compaction` 角色绑定到内置 Agent
  - Agent `fluxdb.agent.default`：压缩触发 10%
  - 只读 SQLite 连接 `验收库` + `orders` 表
- 模型服务：本地 mock `/private/tmp/fluxdb-t035-e2e/mock_model.py`（OpenAI 兼容 SSE，不联网、不耗额度），支持 `normal` / `bad`（截断摘要）/ `slow`（摘要延时）三档，切档脚本 `mock.sh`；末帧带 `usage`，使账本记成 `actual`。
- 启动命令：
  - `bash /private/tmp/fluxdb-t035-e2e/mock.sh normal`
  - `FLUXDB_AI_TEST_ROOT=/private/tmp/fluxdb-t035-e2e/env/config FLUXDB_DEBUG_AI_CHAT=1 FLUXDB_DEBUG_AI_TARGET='{"connection_id":1,"database":"main"}' ./target/debug/fluxdb-desktop`
- 证据收集脚本：`bash /private/tmp/fluxdb-t035-e2e/evidence.sh`（在线备份隔离库 + dump 该会话的轮次/checkpoint/计量/notice/绑定 + mock 日志）。

## 验证结果（会话 `eb849bdf-4c50-49b0-a804-bb8fe9b299ac`）

### ① 独立摘要模型 + 计量归属（正例）

`ai_context_checkpoints`：

```
generation  model         first  last  source_hash
1           model-9766ae  1      14    1331d11a1e107b07   ← mock-summary 档案
2           model-9766ae  14     19    582a9afdeafe6efe   ← 增量：first = 上一条 last
3           model-9766ae  19     23    63bfde28d7c0eacd
4           model-9766ae  23     25    a9aa7ee9eac0b2eb
```

`ai_usage_ledger`（`purpose='compaction'`）：多条 `actual`，其中 `input 916 / output 60`、`254/60`、`126/60` 为成功摘要；`890/7`、`123/7` 为坏摘要（输出仅 7 个 token，符合"被截断"）。

mock 日志（摘要请求只落在摘要档案上，且用的是该档案自己的凭据）：

```
19:47:04 model=mock-summary summary=True auth_header=True
19:52:29 model=mock-summary summary=True auth_header=True
```

同一时刻主请求仍是 `model=mock-main summary=False`；全程**没有**出现 `mock-main summary=True`。

### ② 压缩状态可见

会话内 notice 片段：`*-notice-compaction`（先 `pending`「正在压缩上下文（达到上下文 10%）…」，完成后原地更新为 `complete`「已自动压缩上下文（达到上下文 10%）；原始历史与审计保持不变」）。

### ③ 摘要模型不可用 → 可见说明 + 可见暂停（不静默回退）

停用摘要模型后继续发送：

- 预算内：notice「上下文压缩未执行（摘要模型 model-9766ae… 不可用：所选模型已停用）」，助手回复正常。
- 超预算：`ai_turns.status=failed`，`error_code` 同为上述暂停说明；会话里显示「本轮已暂停：…上下文已超出模型窗口，本轮不会发出超限请求…」。
- `ai_context_checkpoints` 该会话 **0 行**；`ai_usage_ledger` **无** compaction 行；mock 日志**没有**任何 `summary=True`（未回退主模型）。
- 暂停后仍可继续发新任务（会话未被锁死）。

### ④ 截断摘要不覆盖旧 checkpoint

切 `mock.sh bad` 后再发送：会话显示「上下文压缩未完成（摘要疑似被截断）」；核对结果——四行 checkpoint 的 `id / source_hash / last_message_seq` 与验收前**逐字一致**，**未新增 generation 5**；账本新增 `compaction / actual / input 123 / output 7`；mock 日志 `19:54:59 bad_summary=True` + `19:55:11 model=mock-summary summary=True`。

另有一次**传输失败**的真实样本：切换 mock 时打断了在途请求，该轮 `failed / 网络错误: 传输失败`，摘要请求在账本里为 `unknown`，同会话既存 checkpoint 依然未被改动——"摘要请求失败不覆盖旧记录"在错误路径上也成立。

### ⑤ 暗色主题与窄窗（用户执行）

- `shots/d5-panel-notices.jpg`：暗色面板内的压缩成功 notice、坏摘要失败 notice、传输失败 notice。
- `shots/d5-narrow-window.jpg`：窄窗 + 暗色下同一批 notice 的可读性。

## 发现与修复

1. **信息性降级被标成红色"失败"**：历史还不够切点、或摘要模型不可用但仍在预算内时，notice 状态取了 `error`，界面显示"失败"而该轮其实正常继续。已改为：只有"超预算暂停"或真正出错（摘要失败/校验失败/提交失败）才标失败态；其余用非失败态。回归用例：`crates/fluxdb-app/tests/ai_context_compaction.rs::informational_compaction_notice_is_not_marked_as_failure`。
2. **暂停原因存了 3 份**：压缩 notice + `*-notice-compaction-paused` + 助手正文。已去掉中间那份重复片段，保留 notice（承载原因）+ 助手正文（折叠时也可见）。回归用例：`paused_compaction_keeps_two_visible_copies_and_never_marks_success`。

## 命令与结果

- `cargo fmt --all -- --check`、`git diff --check`、`cargo check --workspace --locked --offline` 通过。
- `cargo test -p fluxdb-app --offline --test ai_context_compaction` → **9 passed / 0 failed**。
- `cargo test -p fluxdb-storage --offline --lib model_bindings` → 9 passed。
- `cargo test -p fluxdb-desktop --offline` → 604 passed / 7 failed / 5 ignored；失败项为既有失败（5 项需本地监听的容量用例 + 1 项 HEAD 已复现 + 1 项并行下偶发假超时，单跑通过）。

## 未完成 / 未验证

- 真实第三方模型（公司自建生成模型 / 决策模型）下触发压缩的表现与真实 token 计量。
- "正在压缩上下文…" 的 pending 态截图（本地 mock 响应太快）。
- Windows / Linux 三端；原型逐项对照的浅色主题截图（暗色与窄窗已由用户截图）。
