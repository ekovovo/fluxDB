# FluxDB 测试指南

> 本文以本仓库实际代码、`Cargo.lock` 与**已执行通过**的命令为准。根规则见 `AGENTS.md`。
> 没有实际执行的步骤不记为通过；平台相关结论分别报告，不以单平台推断三端。

## 1. 测试分层

| 层级 | 主要代码 | 测试目标 | 本仓库执行方式 |
| --- | --- | --- | --- |
| 纯逻辑单测 | `fluxdb-core`、各 crate 纯函数 | SQL/参数处理、输入校验、状态转换、错误边界 | `cargo test -p <crate> --locked` |
| 应用/存储/连接器集成 | `fluxdb-app`、`fluxdb-storage`、`fluxdb-connectors` | `AppCommand` → `AppState` / `AppEvent`、持久化、Connector 行为 | 同上；使用临时目录与测试库 |
| GPUI Headless UI 集成 | `fluxdb-desktop` 的**生产 View** | 点击、输入、快捷键、弹框、控件状态与业务结果 | `cargo test -p fluxdb-desktop --locked <filter>` |
| 离屏像素/视觉验证 | 真实渲染结果 | 文字是否绘制、颜色、布局回归 | 尚未建立自动化基线，见 §7 |
| 真实桌面 E2E | 启动后的 FluxDB 进程 | 系统窗口、输入法、文件对话框、平台快捷键 | 人工在真实窗口核验，见 §7 |

选择规则：业务逻辑用低层单测；依赖焦点、事件命中、弹框协作的行为用 Headless UI；系统窗口、辅助功能、IME、原生弹窗及最终观感用桌面 E2E。三者互补，不互相替代。

## 2. 依赖版本与接入原则

### 2.1 当前实际锁定版本

`apps/fluxdb-desktop/Cargo.toml`（`Cargo.lock` 与之一致）：

| 包 | 版本 | 来源 | 备注 |
| --- | --- | --- | --- |
| `gpui-pre`（重命名为 `gpui`） | `=0.3.8` | crates.io | 生产依赖 |
| `gpui-pre-platform`（`gpui-platform`） | `=0.3.8` | crates.io | 生产依赖，开启 `runtime_shaders` |
| `gpui-component` | `=0.7.1` | crates.io | 生产依赖，`tree-sitter` |
| `gpui-kit` | `=0.7.1` | crates.io | **生产依赖** + dev-dependency 加 `test-support` |
| `gpui-fps` | `=0.7.1` | crates.io | 生产依赖 |
| `gpui-base` | `0.7.1` | crates.io | 传递依赖（经 `gpui-pre` / `gpui-component`） |

版本用 `=` 精确锁定：GPUI Kit 生态要求 `gpui-component` 与 `gpui-pre` 精确匹配同一版本，宽松范围会解析出类型不兼容的两套 `Window` / `Entity` / `Context`。

核对命令：

```bash
cargo tree -p fluxdb-desktop --depth 1
cargo tree -p fluxdb-desktop -d          # 应无重复 GPUI 来源
cargo tree -p fluxdb-desktop -e features # 确认 test-support 只在测试构建启用
```

### 2.2 两个必须知道的生态约束

1. **`gpui-kit` 必须是生产依赖。** `gpui-component` / `gpui-base` 的过程宏（`actions!`、`#[derive(Action)]`）经 `proc_macro_crate::crate_name` 解析调用方 crate：**优先 `gpui-kit`，其次回退 `gpui-pre`**。只把 `gpui-kit` 放在 `[dev-dependencies]` 时，普通构建里宏会展开成 `::gpui_kit::…` 而无法解析（`E0433`，报错点落在被展开的宏内部，容易误判为自身代码问题）。生产依赖中声明 `gpui-kit = "=0.7.1"`（不带 feature），仅在 `[dev-dependencies]` 追加同名条目并开启 `test-support`，两者会自动合并，生产构建不受测试 feature 影响。
2. **macOS 需要 `runtime_shaders`。** `gpui-base 0.6.x` 在其非 wasm 依赖中无条件开启 `gpui-platform/runtime_shaders`；`0.7.x` 改为由使用方自选。不开启时 `gpui-pre-apple` 的 build.rs 会调用 `xcrun metal` 编译 `shaders.metal`，把构建机强制绑定到完整 Xcode（仅装 Command Line Tools 会报 `unable to find utility "metal"`）。项目在既有的 `gpui-platform` 依赖上显式开启该 feature，保持升级前的可构建环境。

### 2.3 测试框架 API（以 `gpui-kit 0.7.1` 源码为准）

```rust
use gpui_kit::base::test_support::registered_paths; // 失败时打印已注册元素路径
use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
use gpui_kit::TestAppContext;
```

- `#[gpui_kit::test]`：测试宏。等价于 GPUI 的 `#[gpui::test]`，签名 `fn(&mut TestAppContext)` 或 `async fn(&mut TestAppContext)`。不要 `use gpui_kit::*` 全量导入，避免把该宏与 Rust 内置 `#[test]` 混淆。
- `TestWindowExt`：`find` / `try_find` / `within` / `render_frame` / `click` / `click_at` / `right_click` / `double_click` / `hover` / `scroll` / `drag` / `drag_to` / `press` / `input`。
- `TestAppContextExt::wait_for(handle, timeout, predicate)`：在 `update_window` 之外按可观察条件做有界等待，超时 panic 并打印已注册路径。
- `gpui_kit::init(cx)`：等价于 `gpui_component::init(cx)`。**必须先调用**，它把 `WindowState` 注册为 `Root` 插件，弹框/Sheet/通知层才会挂载。
- `gpui_kit::open_window(options, cx, build)`：与生产一致的窗口入口，内部装好 `base::Root`，返回 `(AnyWindowHandle, Entity<V>)`。
- `TestSupportExt::test_support()`：让普通 `div()` 可被观察；`ObservedElement<E>` 是条件别名类型（未启用 `test-support` 时为 `E` 本身）。给既有元素加它不新增布局节点，也不改变命中区域。
- `ElementSnapshot`：`role` / `path` / `checked` / `indeterminate` / `selected` / `expanded` / `value` / `bounds` / `visible` / `focused` / `disabled` / `label`。

## 3. 测试代码位置

生产 UI 由 `main.rs` 的 `include!` 组织成私有 View，`tests/*.rs` 集成测试无法导入。因此 UI 自动化测试写在 `src/main_parts/` 的 `#[cfg(test)]` 模块内，并在 `main.rs` 用 `#[cfg(test)] include!(...)` 引入：

```text
apps/fluxdb-desktop/src/main_parts/
  tests_ai_ui.rs               # AI 设置页交互回归
  tests_connection_ui.rs       # 新建连接弹框交互回归
  tests/ai_*.rs                # AI 相关细分用例
```

不为测试拆分 `lib.rs`、扩大可见性或重组 `main_parts`。fixture 只负责组件初始化、生产 View 构造、Headless 窗口与隔离目录；不复制按钮回调或重实现业务。

## 4. 编写规范与实测要点

以下每条都来自本仓库实际踩到的边界，不是通则抄写。

1. **先 `render_frame` 再断言。** 交互后重新 `find()`；旧快照不随交互更新。
2. **`input()` 不自动聚焦。** 必须先 `click(field)` 让焦点落到目标输入框。
3. **表单预填默认值时先全选。** 直接 `input()` 会把文本插到光标处（实测得到 `"PostgreSQL LocaUI 测试 PostgreSQLl 1"`）。用 `press("secondary-a")` 全选——`secondary` 在 macOS 是 cmd、其他平台是 ctrl，是跨平台写法。
4. **输入 → 断言业务状态要分两段窗口更新。** `InputState` 的 `Change` 订阅是事件驱动的，生产表单状态在输入事件处理链走完后才更新；同一帧内读 `form` 仍是旧值（实测如此）。
5. **`disabled()` 恒为 `None`，不要依赖它。** 基座 Button 不上报 `aria-disabled`（GPUI 无对应 setter，`gpui-base` 自身测试也断言 `!is_disabled()`）。禁用态改用行为结果验证：点击后任务是否启动、重复点击是否叠加。
6. **异步用 `wait_for`。** 等待最终 UI 条件或业务状态，不要固定 `sleep`。窗口更新回调内不得等待。
7. **作用域用 `within`。** 弹框遮罩这类容器本身不一定被观察（只有其后代注册），此时 `find(MODAL)` 会 panic 而 `within(MODAL)` 可用；确认弹框打开应断言内部控件存在 + `within` 可解析。
8. **失败诊断。** 断言里带上 `registered_paths(window)`，可直接看出是 ID 不对、未观察、还是帧没渲染完。
9. **连接失败分支用不可路由目标**（如 RFC 5737 文档保留网段 `192.0.2.1`），保证确定性失败且不触达本机可能存在的真实服务。

## 5. 隔离与敏感数据

- `FileStorage` 指向 `<temp_dir>/fluxdb-*-<label>-<pid>-<nanos>`，`Harness::drop` 清理；不读写真实用户配置。
- 密码只写入假凭据常量，并断言失败反馈**不回显**密码；快照与日志不含凭据。
- 需要确定失败时用不可路由地址，不引入真实数据库连接。
- 项目现有凭据规则不变：连接密码只存 `credential_ref`；AI API Key 落盘为 AES-256-GCM 密文 + 元数据，数据密钥来自 `config.toml` 的 `secret_key`。

## 6. 已实现的真实 UI 自动化测试

### 6.1 新建连接弹框（生产 UI）

文件：`apps/fluxdb-desktop/src/main_parts/tests_connection_ui.rs`
View：生产 `NavicatMain`（经 `navicat_main_new`）+ 生产 `new_connection_modal`。
入口：`home-new-connection`（真实点击）→ `new-connection-modal` → 各字段。

| 用例 | 模拟的真实交互 | 断言的 UI 状态 | 断言的业务结果 |
| --- | --- | --- | --- |
| `fills_and_tests_a_postgres_connection_through_the_ui` | 点击「新建连接」；点击 `new-connection-kind-postgres`；逐字段 `click` + `input`；点击 `new-connection-test`；进行中再次点击 | PostgreSQL 磁贴 `selected()==Some(true)`；各输入框 `value()` 等于输入文本；密码框 `focused()==Some(true)`；状态区 `role()==Role::Status`、文案离开进行中且不含密码 | 输入经 `Change` 订阅写入 `new_connection_form`；`_test_connection_task` 已启动；进行中重复点击不叠加任务；`AppState.connections` 未新增记录 |
| `shows_validation_error_when_the_name_is_empty` | 点击名称框 → `press("secondary-a")` → `press("delete")` 清空 → 点击「测试」 | 输入框 `value()==Some("")`；状态区文案为 `请填写连接名称` | `test_status` 为 `Error`；`_test_connection_task` 仍为 `None`（校验失败不发起连接） |

隔离：`FileStorage` 指向进程专属临时目录并在用例结束清理；连接目标为 `192.0.2.1:5432`（RFC 5737 文档保留网段），密码为假常量。

为支持稳定定位而做的生产改动（均为既有元素的语义化标识，未插入测试专用元素）：

- `connection_dialog_fields.rs`：每个文本输入框按字段业务身份获得稳定 `ElementId`（`connection-name` / `connection-host` / `connection-port` …）与 `aria_label`；测试结果区加 `new-connection-test-status` + `Role::Status`。
- `connection_dialog.rs`：数据库类型磁贴加 `new-connection-kind-<kind>` + `aria_selected`；弹框遮罩加 `new-connection-modal` 作为 `within` 作用域根。
- `content_views.rs`：首页操作行 `home_action_row` 接受稳定 `id` 并挂 `.test_support()`。

### 6.2 既有 AI 设置页交互测试（`tests_ai_ui.rs` 等）

这些用例早于本次升级，使用 `VisualTestContext` + `simulate_click` 等底层接口，按绝对坐标命中。它们**当前仍通过**，但坐标在视口尺寸变化时会失效；后续应逐步迁移到 `gpui_kit::test` 的稳定 `ElementId` 方式。

## 7. 尚未覆盖 / 需要真实桌面验证

| 模块 | 计划场景 | 当前状态 |
| --- | --- | --- |
| 数据库树 | 展开/折叠、选节点、加载失败 | 未覆盖 |
| SQL 编辑器 | 输入、快捷键执行/取消 | 未覆盖 |
| 查询结果表格 | 选中行、排序、滚动 | 未覆盖 |
| Tab / Dock | 打开、切换、关闭、拖拽 | 未覆盖 |
| 通用 Dialog（`window.open_dialog`） | 打开、提交、取消、Esc、遮罩 | 部分由 AI 设置页用例覆盖，未系统化 |
| 设置页面 | 表单编辑、保存、重载 | 部分由 AI 用例覆盖 |
| AI 模型配置 | 凭据 Keep/Set/Clear、敏感信息保护 | 部分由 AI 用例覆盖 |
| 离屏像素/视觉回归 | 布局、颜色、明暗主题 | 未建立自动化基线 |
| 原生桌面 E2E | 系统文件对话框、原生菜单、输入法、剪贴板、系统快捷键、窗口缩放与 DPI、平台无障碍映射 | **未执行** |

**Headless 与真实 GUI 的区别**：Headless 测试验证真实 GPUI View 的渲染、事件命中、键盘输入与业务结果，但**不会真正弹出可见的操作系统窗口**。原生窗口生命周期、IME 组字与候选窗、系统文件对话框、平台无障碍树映射、最终界面观感必须按平台单独验证，且一个平台通过不能推断三端通过。

### macOS Swift Accessibility 自动化

真实 macOS GUI E2E 优先评估使用 Swift + Accessibility API（AXUIElement），避免依赖截图识别、绝对坐标和逐步调用 AI 工具。

**自动化规则：**

1. 优先使用 `AXIdentifier` 定位控件，通过 `AXUIElementPerformAction` 执行操作。
2. 输入控件优先使用系统支持的 AX 文本操作；不支持时再使用 CGEvent。
3. GPUI 控件应按需提供稳定的 `accessibility_id`，并验证是否正确映射到 macOS 无障碍树。
4. 普通点击、输入、状态读取不得默认使用屏幕坐标。
5. 通过进程 PID 定位 FluxDB，不依赖当前前台应用。
6. 需要使用 CGEvent 时，必须先验证目标窗口及焦点。
7. 将完整测试场景封装成脚本一次执行，避免 AI 每步截图、识别和决策。
8. 默认只在测试失败、视觉回归或最终验收时截图。
9. 测试脚本输出结构化 JSON，包括步骤、断言、失败原因和必要截图路径。
10. 如果无障碍树无法暴露所需组件，应先完善生产控件的无障碍语义，不能使用虚假状态规避问题。

**推荐执行优先级：**

- 业务与常规 UI 交互：`gpui_kit::test`
- macOS 真实 GUI：Swift Accessibility
- 无障碍接口无法执行的交互：CGEvent 兜底
- 视觉变化及失败排查：窗口截图
- 输入法、文件对话框、窗口管理：原生桌面 E2E

如果需要在用户正常操作电脑期间执行焦点敏感的 GUI 自动化，应使用独立 macOS 测试环境。

Swift Accessibility 不能完全消除焦点竞争，必须根据具体控件和系统 API 的实际能力决定是否需要窗口置前。

不得将 GPUI Headless 测试结果冒充真实桌面 E2E 结果。

## 8. 运行命令

已实际执行并通过的：

```bash
# 格式与编译
cargo fmt --all
cargo fmt --all -- --check
cargo check -p fluxdb-desktop --locked
cargo check -p fluxdb-desktop --locked --all-targets
cargo build -p fluxdb-desktop --release --locked

# 全量测试
cargo test --workspace --locked

# 本次新增的 UI 自动化测试
cargo test -p fluxdb-desktop --locked connection_ui

# 单个用例
cargo test -p fluxdb-desktop --locked connection_ui_tests::fills_and_tests_a_postgres_connection_through_the_ui
```

说明：

- UI 测试在 `#[cfg(test)]` 模块内，用 `cargo test -p fluxdb-desktop --locked` 即可选中，不需要 `--test <name>`（本包没有独立 integration test 目标）。
- `--locked` 防止测试时静默改锁文件；本包没有需要手动传入的测试 feature（`test-support` 已由 dev-dependency 启用），**不要**臆造 `--features test-support`。
- 诊断 UI 断言失败时加 `-- --nocapture` 打印 panic 里的已注册元素路径。

### 8.1 已知不稳定用例（非本次引入）

`ai_agent_defaults_tests::ai_ui_agent_defaults_custom_delete_cancel_escape_overlay_then_confirm` 在**全量并行**运行下偶发失败（5s 内确认框未关闭），单独运行稳定通过。已用 `git stash` 对照基线：在升级前的 HEAD 上同样复现（3 次全量运行中 2 次失败，且失败行号与消息一致），因此是既有的并行时序脆弱点，与 GPUI 版本升级无关。修复建议：把该用例也纳入 `dialog_test_lock` 串行域，或改用 `gpui_kit::test::TestAppContextExt::wait_for` 替代 `thread::sleep` 轮询。

## 9. 提交门槛

| 修改类型 | 最低自动化要求 | 人工 / 真实 GUI |
| --- | --- | --- |
| 纯 core/app/storage/connectors | 相关单测 / 集成测试 | 一般不要求启动 GUI |
| UI 行为 / 数据流 | 受影响的 Headless UI 交互测试 + 相关业务测试 | 关键流程与平台行为需验证 |
| UI 纯样式 / 布局 | 编译 + 相关现有测试 | 运行应用检查截图与明暗主题 |
| 系统原生集成 | 应用层或 Headless 能覆盖的子逻辑 | 对应平台真实窗口 E2E |
| GPUI / 组件依赖升级 | 全量编译、核心 UI 回归、`cargo tree -d` 查重复来源 | 至少当前平台主要流程与主题核验 |

禁止通过删断言、扩大超时、固定 sleep、硬编码成功响应或绕过按钮事件让测试变绿。

### 9.1 标准报告

```text
变更：<文件/模块与实际修改范围>
依赖：<GPUI/gpui-component/gpui-kit 的实际版本及来源>
平台：<OS/架构/可用 GUI 环境>
用例：<生产 View、用户操作、预期、实际结果>
已运行：<命令> — PASS/FAIL（数量与失败原因）
未运行：<命令/场景> — 原因
真实 GUI：<实际点击/输入/提交/截图结果，或明确未验证>
问题与截图：<最少必要路径；不得含凭据>
```

## 10. 参考

- 官方测试文档：https://gpui-kit.com/zh-CN/docs/test/
- 官方仓库：https://github.com/longbridge/gpui-kit
- 官方 UI 集成测试：https://github.com/longbridge/gpui-kit/blob/main/crates/kit/tests/ui.rs
- 官方窗口 fixture：https://github.com/longbridge/gpui-kit/blob/main/crates/kit/tests/common/mod.rs
- 依赖升级后核对接口：本文 §2.3 与 `gpui-kit 0.7.1` 源码（`src/test.rs`、`gpui-base 0.7.1/src/test_support.rs`、`gpui-component-macros 0.7.1/src/crate_path.rs`）
- 项目 UI 样式要求：`docs/ui-style.md`
