# T016 / T028 后台执行链路收敛

日期：2026-10-08。分支：`codex/ai-runtime-unify`。实际验证平台：macOS。

## 职责与执行链

桌面提交会话任务 → `AiRuntimeHandle` 有界任务队列 → 同一个 actor →
会话上下文/Provider/工具装配 → `TurnRunner` → 事实持久化与会话投影 →
`ConversationEventReceiver` → 桌面更新。

- `ai_runtime.rs` 负责串行调度、任务句柄、取消、补充与关闭；持久化配置/凭据读取与工具装配不在 UI 线程执行。
- `ai_conversation_runtime.rs` 负责已准备会话任务的装配、提交与异常收尾。只读/写权限、固定目标、Hook、文件授权和审批复核保留。
- `ai_conversation_execution.rs` 从原会话文件拆出，保留 checkpoint、真实工具回执、审批恢复与终态保存。
- `ai_conversation_delivery.rs` 区分可合并增量和终态。每次提交使用容量 4 的更新队列与一个独立终态槽；先读完更新队列，再读终态。满队列或失去 UI 消费者不阻塞 actor 收尾。
- 桌面发送、停止与审批恢复接入运行时；不再直接创建会话执行线程，不再在 UI 层装配 Provider/工具目录。
- 删除无调用方的旧 `run_ai_conversation` 直连模型入口，避免保留重复的凭据/transport 装配。
- 删除旧 `spawn_ai_runtime` / `spawn_ai_runtime_with_tools`、`StartTurn`、`TurnSpec`、`AiEvent` / `AiEventSink` 和独立的 `run_provider_turn` 分支；不留兼容别名。旧原型测试改为唯一 actor 的控制测试与正式会话入口集成测试。
- `block_on_ready` 是主循环、压缩和工具执行共用的能力，从原 actor 文件按能力职责移到 `ai_ready_future.rs`，未复制实现。

## 控制与生命周期边界

- 任务提交与关闭标志切换串行化，关闭之后拒绝提交；已经接受的排队任务按取消状态收尾，不调用其模型来源。
- 活跃任务与排队任务的取消位均可直接置位，不依赖任务队列容量；重复取消幂等。
- 补充按 Turn 隔离，数量与字节有界，溢出显式拒绝而不是丢掉已有输入；主循环在每次模型请求前消费运行中收到的补充。
- 控制锁不覆盖网络、工具或数据库调用。
- 析构请求取消，后台等待线程结束，不在 UI 析构中等待；显式 `shutdown(timeout)` 超时保留线程句柄，不能把超时说成任务已停止。
- GPUI 应用退出回调先请求关闭，再在后台等待。面板隐藏不关闭运行时。
- 不可中断的外部调用仍须等待实际返回；当前同步 HTTP 读超时为 30 秒，而锁定 GPUI 的退出等待上限只有 200ms。本次不修改 transport 和平台退出时限，不能承诺任何阻塞调用均在应用退出前完成；进程退出后仍依赖现有 interrupted/unknown 恢复与禁止未知写重放。

## 自动化验证

- `cargo check --workspace`、`cargo fmt --check`。
- 运行时控制用例 8 项，覆盖队列满、串行执行、排队/运行中取消、满队列关闭、Drop 不阻塞、超时保留句柄、按 Turn 隔离的补充及 sender 尚存时终态槽可见；旧原型专用测试文件已删除。
- `ai_runtime_conversation` 集成测试 6 项：生产提交入口使用本地 HTTP Provider 执行真实 SQLite 读取并保存回执；同一 actor 暂停/批准/恢复写入且重复恢复不重复执行；运行中补充在下一次模型请求生效；满事件队列不阻塞关闭和终态保存；排队取消立即生效且关闭不调用模型来源；Provider 配置失败经事件桥可见并保存 failed。
- HTTP fixture 明确把 accept 后的连接切回阻塞模式并设置超时，避免 macOS 继承监听 socket 非阻塞标志导致请求读取竞态。
- 删除旧原型后使用独立 `CARGO_TARGET_DIR=/private/tmp/fluxdb-runtime-unify-target` 验证，避免另一 checkout 的共享构建缓存混入本 worktree 的类型定义：`cargo check --workspace` 通过，App 单测 713 passed / 1 ignored，App 集成测试 111 passed，Storage 单测 124 passed、集成测试 70 passed，Desktop 全量串行回归 557 passed / 5 ignored，均 0 failed。
- 初次整合阶段：App 单测 724 passed / 1 ignored，集成测试 110 passed；Storage 单测 124 passed，集成测试 70 passed，均 0 failed。
- GPUI 面板交互回归：52 项通过，包含实际模拟发送、停止、流式展示、恢复和明暗主题；Desktop 全量串行回归：557 passed / 5 ignored / 0 failed。

## 真实窗口受阻

已运行隔离 fixture 创建命令及 `cargo run -p fluxdb-desktop`，配置使用
`FLUXDB_AI_TEST_ROOT=/private/tmp/fluxdb-runtime-unify-gui` 和离线 scripted 模型。
但已有 FluxDB 实例持有单实例锁，新进程输出“FluxDB 已在运行，本次启动退出”。
没有关闭用户已有应用，也没有绕过单实例守卫。

另外原生 GUI 自动化服务返回 `Sky Computer Use service startup request failed`，
不能实际点击、输入和截取界面。

因此真实窗口发送/停止/错误/关闭重开/运行中退出未验收，没有截图；
自动化回归与编译成功不能代替这些验收。T016 保持未勾选，T003 的未勾选依赖也不在本次改动中补勾。

## 日志

- `/private/tmp/fluxdb-runtime-domain-final.log`
- `/private/tmp/fluxdb-runtime-desktop-all.log`
- `/private/tmp/fluxdb-runtime-check-final.log`
- `/private/tmp/fluxdb-runtime-integration-1.log` 至 `fluxdb-runtime-integration-3.log`
- `/private/tmp/fluxdb-runtime-gui-start.log`
- `/private/tmp/fluxdb-runtime-controls-final.log`
- `/private/tmp/fluxdb-runtime-stop-final.log`
- `/private/tmp/fluxdb-cleanup-isolated-check.log`
- `/private/tmp/fluxdb-cleanup-isolated-domain.log`
- `/private/tmp/fluxdb-cleanup-isolated-desktop.log`
