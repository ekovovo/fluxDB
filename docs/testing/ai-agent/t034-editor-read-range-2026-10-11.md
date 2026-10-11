# T034 `editor.read_range` 真实窗口验证记录（2026-10-11，macOS arm64）

## 结论

- **用户手动验收确认（2026-10-11，macOS arm64）**：用户在本机真实主窗口完成本轮验收并通过。
  收录范围＝① `editor.read_range` 续读链路（见下）；② 面板开着切换页签后界面上下文跟随**当前**
  页签（T021 缺陷修复）；③ 设置「外观」页不再崩溃（UI 缺陷修复）。三项分别对应提交
  `9489e62`/`4f51400` 与 `208af16`，详细实现与验证见 `docs/ai-agent-tasks.md` 对应条目。
  **本次验收不包含**：T034 其余验收项（界面摘录限额随模型窗口、`Sample.keep_rows` 降采样、
  图片 decoded 像素限制）、明暗主题与窄窗口的原型对照截图——**T034 因此仍未勾选**。
- **`editor.read_range` 端到端链路在真实主窗口验证通过**：编辑器 506 行 → 界面摘录
  超过 400 行触发截断并插入"可用 editor.read_range 读取"提示 → 模型据此发起
  `editor.read_range{offset:300,limit:5}` → 宿主从本轮登记的全文按行窗口取回 → 真实
  回执进上下文 → 助手复述出正确的 5 行。
- 本记录只覆盖这一条链路。**T034 未勾选**：界面摘录限额仍为固定常量、`Sample.keep_rows`
  降采样未做、明暗/窄窗逐项截图未做（见"未产出/未完成"）。

## 环境

- 隔离 fixture：`/tmp/fluxdb-editor-e2e/root`（配置库 `root/config`，两个 SQLite 连接，
  不含任何真实凭据）。
- 模型端点：本地 mock 服务 `/tmp/fluxdb-editor-e2e/mock_model.py` 监听 `127.0.0.1:39107`，
  **不联网、不消耗真实额度**。首轮返回 `editor_read_range` 工具调用，次轮复述工具回执。
- 启动命令（两个终端）：
  - `python3 /tmp/fluxdb-editor-e2e/mock_model.py 39107`
  - `FLUXDB_AI_TEST_ROOT=/tmp/fluxdb-editor-e2e/root/config FLUXDB_DEBUG_AI_CHAT=1 FLUXDB_DEBUG_AI_TARGET='{"connection_id":1,"database":"main"}' ./target/debug/fluxdb-desktop`
- 编辑器内容：`/tmp/fluxdb-editor-e2e/long_editor.sql`，506 行。
- 提问原文：`读取编辑器第 300 到 304 行，并原样列出这 5 行内容。`

## 后端证据（隔离库 `root/config/fluxdb.sqlite`）

发送前后端事实（`ai_turns` join `ai_tool_calls`，通过的那一轮）：

```
turn      ctx_len  tool_id            status     off  lim
8cb28a24  8442     editor.read_range  succeeded  300  5
```

- `ctx_len = 8442`：运行快照 `ui_context` 非空，证明界面摘录已被捕获且因超 400 行触发截断。
- `tool_id = editor.read_range`、`status = succeeded`：工具真实执行成功。
- 参数 `offset=300, limit=5` 与提问一致。

真实回执（`ai_tool_calls.receipt_json`，节选）：

```json
{"tab_ref":"tab-2",
 "lines":["-- filler line 294: 订单口径说明（用于撑行数，不影响语义）",
          "-- filler line 295: 订单口径说明（用于撑行数，不影响语义）",
          "-- filler line 296: 订单口径说明（用于撑行数，不影响语义）",
          "-- filler line 297: 订单口径说明（用于撑行数，不影响语义）",
          "-- filler line 298: 订单口径说明（用于撑行数，不影响语义）"],
 "offset":300,"next_offset":305,"total_lines":506,"has_more":true,"truncated":true,
 "partial_line":null,"untrusted":true,
 "scope":"发送本轮时捕获的编辑器快照，不是实时内容；用户可能已继续编辑"}
```

模型收到的 tool 消息（mock 侧落盘 `requests-1.json`，信封格式）：

```json
{"tool":"editor.read_range","status":"succeeded","truncated":false,
 "result":{"tab_ref":"tab-2","lines":["-- filler line 294: ...","-- filler line 298: ..."],"line_numbers":{"from":300,"to":304},...}}
```

落库的助手最终文本片段与该回执一致（`ai_message_parts`）：

```
界面摘录被截断，我用 editor.read_range 读取中段。续读成功，共取回 5 行：
-- filler line 294: 订单口径说明（用于撑行数，不影响语义）
... -- filler line 298: ...
```

截图：
- `evidence/t034-editor-e2e-window.jpg`：隔离窗口（验收 A 连接、「无标题-查询」页签、右侧询问 AI 面板已展开）。
- `evidence/t034-editor-read-range-success.png`：助手回复列出 5 行、工具卡 `editor.read_range` 状态成功。

## 过程中发现的两个问题（均非产品缺陷，属验证脚手架）

1. **mock 返回了带点号的工具名**：宿主按 OpenAI `function.name` 规则把 `editor.read_range`
   清洗成 `editor_read_range` 下发，mock 回程仍用点号，被宿主按设计拒绝（`模型返回了
   未注册的工具`，fail closed 行为正确）。修正为返回清洗后的名称。
2. **mock 误解析信封**：`ai_loop` 回给模型的是 `{"tool":..,"status":..,"result":<真实回执>}`，
   mock 直接取顶层 `lines` 导致显示"共取回 0 行"。改为读取 `result.lines` 后复述正确。

两次均为 `/tmp/fluxdb-editor-e2e/mock_model.py` 的问题，仓库代码未因此改动。

## 过程中发现的真实产品缺陷（已单独修复）

真实窗口测试暴露 **T021 界面上下文来源页签缺陷**：`context_tab` 只在打开 AI 面板那一刻
记录一次，面板开着时切换到另一个查询页签再发送，`ui_context` 会为空或取自旧页签，
`editor.read_range` 也因不装配而不可用。已修复并单测覆盖，提交 `4f51400`；详细分析见
`docs/ai-agent-tasks.md` 的 T021 缺陷修复条目。

> 本轮最初几次发送失败（`ui_context` 长度为 0）正是该缺陷所致；用户按"先切查询页签、
> 再打开面板"的顺序绕过后才走通链路。

## 视觉验收（明暗主题 + 窄窗，2026-10-11 用户手动执行）

用户按 §39.3–39.4 在本机真实窗口逐项截图核对，三张证据：

| 画面 | 启动方式 | 证据 |
|---|---|---|
| 深色 · 设置「外观」页 | `FLUXDB_DEBUG_AI_SETTINGS=appearance FLUXDB_DEBUG_THEME=dark` | `evidence/t034-dark-appearance.png` |
| 浅色 · 询问AI 面板（含 `editor.read_range` 工具卡） | `FLUXDB_DEBUG_AI_CHAT=1 FLUXDB_DEBUG_THEME=light` | `evidence/t034-light-ai-panel.png` |
| 深色 · 窄窗 900×700 · 询问AI 面板 | 追加 `FLUXDB_DEBUG_WINDOW=900x700` | `evidence/t034-narrow-dark-ai-panel.png` |

核对结果：三种状态均正常渲染、无 panic、无溢出/重叠/文字截断；深色外观页 5 个深色主题色块
（Default Dark / GitHub Dark / Nord Dark / One Dark / Rose Pine）与选中态边框正确；面板时间线、
工具卡、目标徽标（本轮目标 · 验收 A · 只读）、输入框在浅色与窄窗深色下均可用。原型
`docs/prototype/ai-agent-ui.html` 不覆盖外观页，该页对照 `docs/ui-style.md` 核对。

## 验收范围说明（用户确认接受）

- **图片 decoded 像素/活动数量限制：当前不适用。** 代码里没有任何图片进入模型的通道
  （`ModelMessage` 仅 `role`/`text`/`tool_call_id`/`tool_name`/`tool_calls`，
  `crates/fluxdb-core/src/parts/ai/mod.rs:66`；`PartKind` 亦无图片类型），
  该约束没有可作用对象。设计 §13.2/§15 本身未要求图片上下文。经用户确认，本条按
  "无图片路径 → 不适用"记录，不实现图片通道、也不宣称已有该约束。
- **界面摘录 16 KiB / 400 行为有意默认。** 设计 §13.2 原文是"默认单段上限 16 KiB / 400 行
  （**可**按模型上下文调大）"，属可选增强；按最终模型窗口重算的**上下文总预算**已由
  `should_compact`/`ai_conversation_turn_messages` 实现（T069），此处保持默认值不额外改动。
- **`Sample.keep_rows` 降采样不做。** `db.query` 产出时已只带 `RECEIPT_SAMPLE_ROWS = 5` 行
  样本 + `rows_omitted` + `result_ref`，通用投影层仅兜底，收益不足以引入额外复杂度。

## 未产出 / 未完成

- **T034 未勾选**：`editor.read_range` 只是 T034 的一项。其余验收项未完成：
  - 界面摘录仍是固定 `16 KiB / 400 行`（`ContextLimits::default()`），未按最终模型窗口调整；
  - `ToolOutputShape::Sample.keep_rows` 目前只参与兜底丢弃顺序，未做"样本行数本身超预算时
    进一步降采样"；
  - 图片 decoded 像素/活动数量限制目前无图片进模型的路径，约束尚未真正生效；
  - ResultRef 媒体引用、路由事件落库未做；
  - **明暗主题与窄窗口的原型对照截图**未做（本记录只有浅色、常规窗口）。
- 平台：仅 macOS arm64。Windows / Linux 未验证。
- 未能自动化的部分：合成的 AppleScript 点击在 GPUI 窗口上不可靠（本次改用真实人工
  操作 + 后端事实行核对）；打开 AI 面板/切换页签的自动化仍无稳定通道。
