# T071 模型真流式与页面性能

平台：macOS arm64。分支 `codex/ai-executor-plugins`，实现、测试和独立 `target/` 均在本 worktree；不访问真实模型、数据库或用户配置。

## 参考源码（先阅读，再实现）

| 项目/固定版本 | 阅读位置 | 借鉴点 |
| --- | --- | --- |
| Pi `a276dabe57911253350bffb93cb7d7aff6a73261` | `packages/agent/src/agent-loop.ts`、`packages/coding-agent/src/modes/interactive/interactive-mode.ts` | Provider 事件即时透传到活跃消息，最终消息与中间事件分离 |
| 同上 | `packages/tui/src/tui.ts` 的 `requestRender`/`scheduleRender` | 同一节拍内合并重绘（16ms），不让每个 token 单独重绘 |
| Goose `591edd47cf2cfea4957d720c607cf2a4def8673d` | `ui/desktop/src/hooks/useThrottledStreamingText.ts` | 最新数据与渲染文本分离，首片立即、冷却窗口合并、结束取最新内容 |
| 同上 | `ui/desktop/src/components/GooseMessage.tsx`、`ProgressiveMessageList.tsx` | 普通正文 50ms、长正文 250ms 降低 Markdown 解析频率；稳定消息标识与历史加载分批 |
| 本仓库锁定 GPUI | `gpui-base 0.7.1` 的 `text/state.rs::push_str` | 直接复用 Markdown 增量解析与实体，不另造 Markdown 渲染器 |

不照搬 Pi 的终端重绘或 Goose 的 React 状态模型。fluxDB 沿用已有 actor、GPUI 主线程与有界回桥；当前网络读取仍是后台 ureq，同步 IO 的取消/等待受 transport 超时约束，不声称执行器能抢占阻塞 `poll`。

## 交付边界

- Core `GenerationProvider::stream_events` → 生产 HTTP Provider → SSE 解析点 → 用量宿主 → 主循环 → 会话增量 → 生产 Markdown View。标题/摘要的 `stream` 收集接口保留，主模型不再收集完整数组后重放。
- HTTP SSE 热路径不保留整段 `Vec<ModelEvent>`；正文/摘要的待发增量各占一个合并槽，总字节最多 4 MiB。结构队列容量 4、终态另槽，终态不被高频增量挤掉。
- 网络事件到达即合并进回桥槽，不在发送侧用“等下一个事件”代替定时冲刷。前台仍每 16ms 泵回桥，但普通增量不额外通知全列表重绘；Markdown 分别以 50ms / 250ms 追加批次，终态立即冲刷。
- 文本持久化每 300ms 一次检查点；正文热路径不再每次检查点重读整段会话。操作边界/终态发布权威投影。
- 首片后失败保留半截正文、不自动重试拼接；EOF/`[DONE]` 不代替模型结束事实；未完整结束或截断不得执行工具。请求用量在取消丢弃 future 时也结算，不遗留 pending。
- 不新增页面 AI 上下文读取能力，不扩大页面文本/数据出站范围；密码、Key 和加密载荷不进入新增事件。

## 已执行的针对性验证

所有 Cargo 命令使用 `CARGO_TARGET_DIR="$PWD/target"`、`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`；初次验证曾使用共享 target，后因缓存产物混用切到独立目录，最终结果以独立目录为准。仅需绑定回环端口的测试在沙箱外执行。

| 命令/场景 | 结果 |
| --- | --- |
| `cargo test --locked -p fluxdb-app --test ai_live_streaming -- --nocapture` | 4 passed；覆盖三个协议首片先于响应结束、EOF、真实取消、挂起 future 用量结算 |
| `cargo test --locked -p fluxdb-desktop real_sse_is_visible -- --test-threads=1` | 1 passed；真实点击发送，明暗主题均在尾帧闸门释放前看到生产 Markdown View 中的首片；暗色主题真实点击停止，断言只保留半截正文并落盘为 interrupted，草稿不丢失 |
| `cargo test --locked -p fluxdb-desktop streaming -- --test-threads=1` | 2 passed；既有 Markdown 流回归、10,000 次 UTF-8 增量合并与长正文降频 |
| 新增回桥单测 | 10,000 次多字节增量只留下一个待发正文槽；队列已满、没有后续 token 时仍能取到完整尾片，终态最后交付 |
| 新增重试单测 | 首片后超时只调用一次；首片前连接失败仍有限重试且正文不重复 |

最终补测 `cargo test --locked -p fluxdb-app --test ai_live_streaming --test ai_plan_plugin_gating --test ai_telemetry_plugin_gating`：4 + 1 + 1 passed。包含断流的生命周期诊断失败计数，完整工具 JSON 缺少模型结束事件时拒绝执行；纯逻辑主循环另有直接回归用例。

`cargo fmt --all`、`cargo check --locked --workspace`、`git diff --check` 已执行并通过。`cargo test --locked -p fluxdb-app --lib`：908 passed / 10 ignored。

执行了 `cargo test --workspace --locked --no-fail-fast -- --test-threads=1`，**未全绿**：

- app 集成 `ai_mcp_install::removal_cleans_legacy_preapproval_but_keeps_other_permissions` 失败；本聊天前序已在干净基线复现。
- desktop：608 passed / 3 failed / 5 ignored。失败为 `ai_ui_default_model_selector_persists_runtime_setting`、`ai_chat_model_selection_disabled_model_error_preserves_selection_and_input`（前序已在干净基线复现），以及 `ai_ui_agent_defaults_custom_delete_cancel_escape_overlay_then_confirm`（前序也出现过并单独复跑通过，本次在关闭确认框等待超时）。
- 新增流式、重试、合并槽、两插件门控以及 GPUI 真 SSE 用例均通过；其余失败不隐藏、不归为本次验收通过。完整日志本机 `/tmp/stream-workspace-tests.log`，最终定向日志 `/tmp/stream-final-integration.log`、`/tmp/stream-final-ui.log`。
- 编译保留既有第三方 native 库最低 macOS 版本链接警告，以及 `block 0.1.6` 的 future-incompat 提示；没有升级依赖以掩盖警告。

## 可见桌面验收

执行了 `cargo run --locked -p fluxdb-desktop`，使用 `FLUXDB_AI_TEST_ROOT=/tmp/fluxdb-stream-ui-20261016`、`FLUXDB_PROFILE=stream-e2e` 及无凭据的回环模型配置。启动日志显示初始化与恢复完成。

原生桌面操作服务在枚举和绑定已启动窗口时均返回 `Sky Computer Use service startup request failed`，无法执行可见窗口内的点击、输入、截图；因此仅记为“启动已验证，可见桌面 E2E/像素观感未验证”。已关闭本次创建并核对工作目录的隔离应用与回环服务，不关闭其他实例。

Windows/Linux、真实第三方 Provider、系统 IME/原生无障碍映射未验证。不把单元/Headless 结果描述为三端 E2E 通过，也不以这些计数宣称已有 FPS/内存峰值基准。
