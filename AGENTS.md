# AGENTS.md

适用于整个仓库。

## 工作原则
- 回答简洁直接，保留必要技术信息；只做任务需要的修改，不顺手扩建或重构。
- 动手前读清相关实现和调用链，优先复用现有代码、标准库、平台能力和已安装依赖；修复根因，避免临时兜底、硬编码特例和过度封装。
- 不削减边界校验、错误处理、数据丢失防护、安全与可访问性；关键分支、异常、外部调用和异步/批量处理按需补日志，复杂逻辑添加简洁中文注释。
- 复杂逻辑或 bugfix 可使用 TDD，简单改动不强制；完成后合并不必要的小函数。
- 合理使用 Rust trait 表达稳定的能力边界：数据库、平台或存储存在多种实现，或需要替换外部依赖进行测试时，优先复用已有 trait，必要时定义职责单一的小接口；接口归属使用该能力的领域/应用层，具体实现留在对应实现层。编译期确定实现时优先泛型或 `impl Trait`，运行时需要异构实现时使用 `dyn Trait`；简单逻辑和无替换需求的单一实现保留具体类型，不为假想扩展引入 trait。
- 默认禁止 OpenSpec / Superpowers，除非用户在当前请求中明确要求。

## 命名与注释
- 函数、类型和模块按实际职责与语义命名，不按首次调用的页面、功能、调用方或临时需求命名；可复用逻辑不附加无必要的场景前缀或后缀。确有业务约束、平台差异或专属行为时保留限定，避免泛化名称掩盖真实限制，也避免 `handle` / `process` / `utils` 等含糊命名。
- 区分通用能力与场景编排：场景入口可以使用业务名称，底层可复用逻辑按操作、对象及必要约束命名；新增前先搜索已有等价能力，不因调用场景不同复制实现或增加仅转发的专用包装。不为假想复用扩大接口、增加参数或抽象层。
- 注释说明行为契约、前置条件、边界、副作用及设计原因，不把当前调用场景写成用途限制；场景示例明确标为示例，只有代码确实依赖该场景时才写“仅用于某场景”。避免重复函数名或逐句翻译代码。
- 提交前检查新增或修改的名称与注释：换一个符合相同契约的调用方，描述是否仍然准确？若不准确，先判断是名称或注释过窄，还是实现确实专用，再修正；本次改动暴露出的误导性描述同步更新，不顺手批量重命名无关代码。

## 目录与分层
- `apps/fluxdb-desktop/`：GPUI UI，只负责渲染和交互；新增功能放在 `src/main_parts/`，按页面或交互职责组织。
- `crates/fluxdb-app/`：应用状态与业务编排；UI 通过 `AppCommand` 发起操作，通过 `AppState` / `AppEvent` 表达状态变化。
- `crates/fluxdb-core/`：通用领域模型、错误类型和 Connector 接口，不依赖 UI 类型。
- `crates/fluxdb-connectors/`：数据库连接与操作，按数据库类型或共享能力拆分。
- `crates/fluxdb-storage/`：配置与连接持久化；配置不得保存明文密码，只保存 `credential_ref` 或非敏感选项。落盘分两类：`connections` / `history` 是真表（见 `src/parts/connection_store.rs`、`src/parts/history_store.rs`）——只有跨数据库类型通用的字段才建列，类型专属字段进 `extra_json`，新增数据库类型不改表结构；其余设置类数据（settings、sidebar layout、ER 视图、备份记录等）仍走 kv 表的 load-all / save-all JSON blob。
  - **AI 管理实体例外**（设计文档 §42）：AI 模型、Agent（含直接编辑的提示词，不单独做模板/多版本）、插件实例/绑定、MCP 设置、运行保护策略（沿用预算策略表名，不含金额或累计 token 上限）是独立可编辑对象，各用独立表存储（`ai_model_profiles`、`ai_agents`、`ai_plugin_instances`、`ai_agent_plugin_bindings`、`ai_budget_policies`、`ai_mcp_settings`、`ai_runtime_settings`，DDL 以设计文档 §42.10 与 `crates/fluxdb-storage/src/parts/ai_platform/schema.rs`（结构的唯一来源）为准），每实体独立 `revision` 做对象级 CAS，不再整体塞进 kv 的 `ai.config.v1` blob；通用身份/引用/版本/状态建列，Provider/插件专属参数进受应用层校验的 JSON。
  - 模型 API Key **不得明文落盘**：AES-256-GCM 密文及加密元数据（nonce、cipher_version、key_id、secret_revision）保存在对应 `ai_model_profiles` 行的加密列，复用连接凭据同一把数据密钥（`config.toml` 的 `secret_key`），数据密钥不进入任何模型表；不使用独立 `ai_credentials` 表作为模型凭据的正常存储。
  - 普通配置 DTO、聚合视图、导出、日志、事件与运行快照**不得包含**凭据明文或 ciphertext/nonce/tag 加密载荷；UI/Runtime 只拿到非敏感模型配置与凭据状态（`NotRequired`/`Missing`/`Configured`/`Unavailable`），保存 API Key 走显式 Keep/Set/Clear。连接密码、history、非 AI kv 与通用 Settings TOML 的规则不变。
- App、Core、Connector 新逻辑优先放在各自 `src/parts/` 对应职责文件；UI 不得直接调用数据库驱动、拼接数据库 SQL、读写配置或保存密码。
- `main.rs` / `lib.rs` 仅保留入口、模块声明、导出、启动和少量胶水代码；已有 `include!` 拆分保持现有作用域，不顺手迁移模块。
- 源码超过约 800 行应评估拆分；超过约 1200 行且需新增功能时，必须先拆出清晰职责。结构拆分尽量与行为修改分开，优先 `pub(crate)` 或更小可见性。

## AI 运行时与执行器
- AI 运行时（`crates/fluxdb-app/src/parts/ai_*`）的异步执行器归 `crates/fluxdb-app/src/parts/ai_executor.rs`：`block_on` 必须在**真实 waker 上停放/唤醒线程**（park/unpark），按取消与上限收尾；不使用固定轮询次数假装完成，也不依赖外部 IO 反应堆。
- 取消与超时对所有 await 点生效：等待期间 `CancelFlag` 置位、达到 wait 上限或 provider/transport 自身 deadline 都必须尽快返回；执行器线程是后台线程，任何等待都不得阻塞 UI 主线程。
- 事件的实时性靠**即时发布**而不是批量回放：模型增量、工具执行中进度、审批与回执在产生点写入有界回桥通道，UI 线程通过有界通道并发消费；不先攒进 `Vec` 等工具结束后再补发。
- 所有通道有界：进度/事件回桥容量设上限，溢出时显式拒绝或按既定合并策略处理，不无界增长、不静默丢弃终态。
- 不在热路径按任务新建线程：执行仍在既有 AI 运行时 actor 线程内编排；确需并发时使用固定数量的后台 worker，并保证数量与队列有界。
- 生产依赖只从 `Cargo.lock` 已有版本选择（如 `futures` / `smol`），先核查锁文件再决定直接依赖与 feature；不引入第二个自主 Agent 循环，不在本仓库外加平台专属运行时。

## 跨平台
- 目标平台为 Windows、Linux、macOS；新增和修改功能必须考虑三端兼容，不默认依赖当前开发机的系统行为。
- 路径使用 `Path` / `PathBuf` 和平台目录能力，不硬编码路径分隔符、用户目录、盘符或系统临时目录；外部命令、快捷键、窗口行为及系统服务需考虑平台差异。
- 优先使用现有跨平台抽象与依赖；平台专属实现集中隔离，必要时使用 `cfg`，并提供其他平台实现或明确的不可用反馈，避免无条件引入平台专属依赖。
- 验证时说明实际检查的平台与未验证范围；单平台通过不能视为三端通过。

## UI 约定
- 修改 UI 前检查 `Cargo.lock` 中实际锁定的 `gpui-component` / GPUI Kit 版本和项目已有封装（当前锁定 `gpui-component 0.7.1`、`gpui-kit 0.7.1`、`gpui-pre 0.3.8`）；通用控件必须复用组件，不得用 `div()` 替代或新增重复本地控件。仅在组件缺失或经验证无法满足语义时自行组合，并记录原因；普通布局容器可用 `div()`。
- 依赖 GPUI 生态的宏（`actions!`、`#[derive(Action)]` 等）由 `gpui-component` / `gpui-base` 的过程宏经 `proc_macro_crate` 解析：优先 `gpui-kit`，其次回退 `gpui-pre`。因此 `gpui-kit` 必须是**生产依赖**；只放在 `dev-dependencies` 会让宏在普通构建中展开成无法解析的 `::gpui_kit::…`。
- macOS 构建需在 `gpui-platform` 上显式开启 `runtime_shaders`：`gpui-base 0.6.x` 曾无条件开启，0.7.x 改为使用方自选，不开启则 build.rs 调用 `xcrun metal`，把构建机强制绑定到完整 Xcode。
- 颜色统一走 `UiColors`，兼容明暗主题；功能图标使用 `AppIcon` / `app_icon(...)` / `app_icon_box(...)` 与 lucide 风格 SVG，不用文字字符代替。
- 弹框接入关闭按钮、`Esc` 和遮罩关闭；弹框、浮层、下拉菜单均支持外部点击关闭，内部点击阻止穿透。
- 按钮及按钮式控件提供 `cursor_pointer()`、hover，组件支持时提供 active；按钮圆角、输入框和菜单样式遵循 [UI 样式规范](docs/ui-style.md)。
- 请求数据、数据库和文件操作等耗时任务必须显示 loading/disabled，不阻塞 UI；数据区域刷新也需明确 loading。
- 轻量反馈统一使用主窗口 `show_message(...)`，单槽替换并重新计时，不散落 `push_notification` 或自绘提示。
- 新增页面或页面功能时，必须检查其“当前页面 AI 上下文”是否已接入，并向用户明确说明 AI 能看到哪些定位、结构、文本、数据与状态，以及未接入或不可读取的内容。页面尚未接入时，询问用户是否需要让 AI 获取该页面上下文；在确认前不得静默扩大读取或上传范围。接入时复用当前页面引用协议，按内容选择全文、摘录或结构化摘要，明确截断、版本和续读能力；密码、API Key 等敏感配置始终禁止进入上下文，业务数据值只能经有界只读工具按需读取，不得塞进首条消息。

## 自动化测试约定
- 测试分为纯逻辑单测、App/Connector/Storage 集成测试、GPUI Headless UI 集成测试、离屏视觉验证、真实桌面 E2E；按变更的实际风险选择，完整执行方式见 [测试指南](docs/testing.md)。
- UI 交互回归使用 `gpui_kit::test`（`#[gpui_kit::test]`、`TestWindowExt`、必要时 `TestAppContextExt`），测试生产 View 和真实事件链；不以直接调用回调或 AppCommand 处理函数代替点击测试。生产 UI 只由 `main.rs` 的 `include!` 组织、私有 View 无法被集成测试导入时，测试写在 `src/main_parts/` 的 `#[cfg(test)]` 模块内，不为测试拆分入口或扩大公开 API。
- Headless 断言前先 `render_frame`；交互后重新查询快照，旧快照不随交互更新。`input()` 不自动聚焦，必须先真实点击输入框；表单预填了默认值时先全选（跨平台写法 `secondary-a`，macOS 为 cmd、其他平台为 ctrl）再输入，否则文本会插到光标处而非替换。
- 输入框的 `Change` 订阅是事件驱动的：业务状态在输入事件处理链走完后才更新，因此「输入 → 断言业务状态」要分成两段窗口更新，不要在同一帧内断言。
- 交互控件使用稳定 `ElementId`，重复 ID 用已有父级 `within` 作用域定位；列表节点优先使用稳定业务 ID，不依赖易变下标、随机 ID 或绝对坐标。普通 `div()` 需 `.test_support()` 才可被观察；该调用在未启用 `test-support` 时是恒等变换。已有稳定 ID 时不新增测试专用元素或容器。
- 异步操作在窗口更新之外用 `wait_for` 做有界条件等待，等待最终 UI 或业务状态而非执行器空闲；不依赖任意固定 `sleep`。
- 基座 Button 不上报 `aria-disabled`，`disabled()` 返回 `None` 属正常，不代表按钮可用或不可用；禁用态改用行为结果验证（点击后任务是否启动、是否重复提交）。
- UI 自动化同时断言用户可见状态和 AppCommand / AppState / AppEvent 等业务结果；按风险覆盖成功、错误、loading/disabled、空态、焦点、快捷键及弹框开关。
- 连接器使用既有接口和受控 Mock，存储使用隔离临时目录或测试库；不得触达生产数据库、覆盖真实用户配置或把密码、API Key、密文及加密元数据输出到日志、截图、快照或报告。需要确定失败分支时用 RFC 5737 文档保留网段等不可路由目标，而不是本机可能存在的服务。
- Headless 事件测试不等于像素检查或真实桌面 E2E；系统输入法、原生窗口、文件对话框、无障碍平台映射和最终界面观感仍需按平台验证。跨平台结果分别报告，不以单平台结果推断三端通过。

## 验证
- Rust 代码修改后运行 `cargo fmt --all` 和 `cargo check --locked`；验收报告注明具体命令，检查是否完成。
- 修改 core/app/storage/connectors 逻辑后运行相关测试；影响范围不明确时运行 `cargo test --workspace --locked`。
- GPUI 依赖升级必须额外验证：`cargo tree -d` 确认无重复 GPUI 来源，且 macOS / Windows / Linux 三端至少各完成一次 `cargo check`。
- UI 行为或数据流修改优先运行受影响的 GPUI Headless UI 自动化测试，必要时加入回归用例；使用真实 View 模拟点击、输入、快捷键、滚动或拖拽，并检查用户可见反馈与业务结果；接口以当前锁定版本为准。
- UI 修改需运行 `cargo run -p fluxdb-desktop` 检查真实界面；关键用户流程和系统相关功能必须在可见窗口实际点击、输入、提交并检查结果，覆盖相关 loading/disabled、空态、错误态。纯视觉修改按需重点核验截图与主题；只编译、进程内测试或只启动成功均不能冒充真实 E2E 通过。
- 结合实际界面截图，对照设计稿（如有）、需求和 `docs/ui-style.md` 检查 UI 还原度、布局间距、文字、颜色及控件状态，验证明暗主题。
- 按模块测试、修复并复测；截图少截、裁准，首次读取前缩放并压缩，避免重复加载大图。允许委派 UI 测试子 agent，共享 GUI 操作须串行，返回简短结论与必要截图路径，完成后关闭。
- 验收仅汇总场景、结果、实际执行的命令、平台、问题与必要截图路径；无法启动、截图或执行测试时明确阻碍及未验证项，不得宣称通过。
