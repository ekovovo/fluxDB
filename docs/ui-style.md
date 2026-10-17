# UI 样式规范

适用于新增和修改桌面 UI，是根目录 [AGENTS.md](../AGENTS.md) 的专项细则。修改 UI、页面 AI 上下文或 GPUI 依赖前阅读相关章节；验证范围见根文件“测试与验收”。

## 组件与依赖
- 修改前核对 `Cargo.lock` 和已有封装。当前锁定 `gpui-component 0.7.1`、`gpui-kit 0.7.1`、`gpui-pre 0.3.8`；以实际发布版接口为准，不照搬上游 main。
- 通用控件必须复用组件，不能用 `div()` 替代或新增重复本地控件；仅当组件缺失或经验证无法满足语义时自行组合，并记录原因。普通布局容器可用 `div()`。
- `actions!`、`#[derive(Action)]` 等 GPUI 生态宏由 `gpui-component` / `gpui-base` 的过程宏经 `proc_macro_crate` 解析，优先 `gpui-kit`，其次回退 `gpui-pre`。`gpui-kit` 必须是生产依赖；只放开发依赖会导致普通构建展开出无法解析的 `::gpui_kit::…`。
- macOS 需要在 `gpui-platform` 显式开启 `runtime_shaders`：`gpui-base 0.6.x` 曾无条件开启，0.7.x 改为使用方自选；不开启则 build.rs 调用 `xcrun metal`，把构建机强制绑定到完整 Xcode。
- GPUI 升级须额外运行 `cargo tree -d` 确认没有重复 GPUI 来源，且 macOS、Windows、Linux 原生环境各完成一次 `cargo check`；不能用单平台或跨目标编译替代三端验证。

## 主题、交互与反馈
- 颜色统一走 `UiColors`，兼容明暗主题；图标使用 `AppIcon` / `app_icon(...)` / `app_icon_box(...)` 与 lucide 风格 SVG，不用文字字符代替。
- 弹框接关闭按钮、`Esc` 和遮罩关闭；弹框、浮层、下拉菜单支持外部点击关闭，内部点击阻止穿透。
- 按钮及按钮式控件提供 `cursor_pointer()`、hover，组件支持时提供 active；具体圆角、输入框与菜单样式见后续章节。
- 数据请求、数据库和文件操作等耗时任务显示 loading/disabled，不阻塞 UI；数据区域刷新也要明确 loading。
- 轻量反馈统一使用主窗口 `show_message(...)`，单槽替换并重新计时，不散落 `push_notification` 或自绘提示。

## 当前页面 AI 上下文
- 新增页面或功能时检查是否接入，并向用户说明 AI 可见的定位、结构、文本、数据与状态，以及未接入/不可读内容。
- 页面尚未接入时先询问是否需要接入，确认前不得静默扩大读取或上传范围。
- 复用当前页面引用协议，按内容选择全文、摘录或结构化摘要，明确截断、版本和续读能力；不另造上下文协议。
- 密码、API Key 等敏感配置始终禁止进入上下文；业务数据值只能经有界只读工具按需读取，不得塞进首条消息。

## 按钮
- `gpui-component::Button` 默认使用 `.rounded_md()`；明确的图标按钮或组件语义要求可例外。

## 表单输入框
- 使用 `gpui-component::Input`，放在自定义外框内；外框高度约 34px，宽度按表单语义稳定约束。
- 外框使用 `rounded_md`、`border_1`、`colors.input_bg`，默认边框为 `colors.border`，hover 使用主题适配的增强边框。
- focus 边框：明亮主题 `rgb(0x111111)`，暗黑主题 `rgb(0x8ab4ff)`；hover 不得覆盖 focus。
- 内部 Input 使用 `appearance(false)`、`focus_bordered(false)`、`w_full()`、`h_full()`，字号约 13px，保证光标垂直居中。

## 右键菜单
- 保持紧凑宽度，文字使用略重字重。
- 二级浮层贴齐主菜单右侧边缘，避免明显水平缝隙或垂直错位。
