# fluxDB 原生 AI Agent：实施任务清单与验收

> **2026-10-11 用户确认**：只统计会话 Token，无金额或累计 Token 上限；模型价格保留供路由。历史费用记录已由主设计 §30 与当前 T030 口径取代。

- 日期：2026-10-03；状态：实施前清单。
- 对应完整设计（唯一设计入口）：[ai-agent-design.md](/Users/shining3d/fusuwei/code/fluxDB/docs/ai-agent-design.md)。
- 本轮只输出文档，不代表已有 AI runtime、管理表、插件或 E2E 功能。
- 下列任务按依赖推进，不要求一次性实现；结构拆分、行为修改、测试证据分开。

## A. 不可改变的验收约束

1. 原生内部 Agent；不接入外部 Agent 产品作为主执行者。
2. 主 Agent 能自主探索/调用/修正/验证，不是分类后套固定流程。
3. 完全无 Jev 的核心运行完整；Jev 故障不阻断、不持续重复等待。
4. AI 管理实体与运行数据独立表；模型密文在 ai_model_profiles 行；模型/MCP 通用引用使用角色绑定表而非 plugin JSON。
5. 复用连接 AES-256-GCM 和同一数据密钥；不读取/写系统钥匙串，不引入 ai_credentials。
6. 配置/事件/日志/导出/快照无明文 Key、nonce/ciphertext；显式 Keep/Set/Clear，CAS 和失败回滚。
7. 所有插件只能在内核权限/执行保护内运行；不能伪造工具成功、跳过审批。
8. 每个 UI E2E 步骤只看当前小图；先压缩本地图片，再输入视觉上下文，每步保存文本 checkpoint。
9. Rust 变更 cargo fmt/check；业务变更相关测试；UI 变更启动主窗口和实际交互；记录当前开发平台的实际结果。
10. 所有框架引入先锁版本/查许可证/验证，不能根据main文档猜发布版功能。
11. 本期只实现内置插件/自动可选Jev，第三方只留接口，不写安装器/Wasm。无历史AI数据，不做迁移引擎。
12. 真实DB/UI/MCP测试用用户已授权的现有Docker MySQL/PostgreSQL/Redis连接；按本run namespace隔离，设置“大模型/工具与MCP”菜单和持久时间线为验收重点。

## B. 任务记录规范

**平台验收分工（2026-10-05）**：本清单全部实施任务完成后，Windows/Linux 的启动、功能、UI、截图与 E2E 最终验收由用户自行执行，不作为任何单项任务或代理交付的完成门槛，也不因未执行而保持任务未勾选。实施阶段仍须保证跨平台兼容，并完成当前开发平台（macOS）的相关测试与真实窗口验收；不宣称其他平台已通过。此规则覆盖所有任务及历史记录，不免除其他功能、安全、交互和视觉验收要求。

每个 Txxx 结束记录：改动文件、实际命令与结果、平台、用例/step、证据和 checkpoint 路径、未验证范围。任务勾选仅在验收真实通过后，不因代码写完就勾选。 UI任务统一执行主设计§39.3–39.4：功能齐全、交互回归、真实窗口E2E、截图还原度及修复复测均满足后才勾选；失败/受阻保持未勾选，已勾选但不满足者撤回并保留历史记录。范围内差异不能仅记录或推迟到T064。

- P0：核心闭环与安全/数据正确性，不能跳过。
- P1：用户所需智能优化、Hook/BI/插件体验。
- P2：可安装插件、MCP 与新能力扩展。
- 依赖标记 `Txxx`；并列任务可以在互不重叠写集下推进，但本清单不自动授权子 Agent。
- 初始代码落点按设计 §7；具体文件名可小幅调整，但职责/可见性/调用方向不改变。

### B.1 每任务验收执行规则

每项任务下的“验收”都是独立通过条件，不是可任选示例；执行前逐条转成用例，记录输入/前置状态→动作→预期→实际→证据。成功、失败及适用的取消/恢复路径分别测试，不以整体 `cargo test` 通过替代具体断言。下方“测试方式”说明执行层级；延期任务仅在恢复实施时执行，不扩大本期范围。

UI原型：`docs/prototype/ai-agent-ui.html`（本机 `/Users/shining3d/fusuwei/code/fluxDB/docs/prototype/ai-agent-ui.html`），配套同目录JS/CSS；页面归属和实际打开原型→同尺寸/主题/状态截图→逐项差异→修复复测按设计§39.3.1。每个UI任务独立完成GPUI交互、真实窗口操作和视觉对比，不推迟到T064。

数据驱动页面另执行设计§39.5：预置已知记录→正常入口实际可见，UI保存→不手动刷新即更新，返回/重启→仍可见；区分loading/空/有数据/失败并检查迟到响应。测试经过生产加载、AppState、渲染快照及实际控件，不只查SQLite或给孤立组件塞数据。证据需包含目标记录的非敏感ID/标记、预期与实际字段、后端断言及有数据截图；只截空态不得勾选。

真实模型按设计§37.3：主Agent用SQLite已配置“公司自建”，Jev用“决策模型”；通过现有宿主凭据接口在进程内AES认证解密后直接供Provider请求使用，不打印或导出Key/加密载荷，不要求用户重填，不修改原配置。真实调用显式启用且有调用/迭代/时间保护，普通单测默认scripted；真实成功与故障注入分开报告。

## C. 文档阶段产物

- [x] D001 汇总用户需求、内嵌 Agent 和非目标；设计 §1–3。
- [x] D002 核对现有执行/存储/加密调用链及本地 Pi/Goose commit；设计 §4–5。
- [x] D003 写出模型/插件/工具/产物/运行时契约、完整独立表 DDL；设计 §6–36、§42。
- [x] D004 旧基线27表/31项、MCP增补31表/44项保留为历史；本轮删除迁移并新增parts/UI/audit，33表/65项通过，全部是文档/内存SQLite检查。
- [x] D005 按八点需求修订内置插件/自动Jev、菜单、美观时间线、消息parts、必经审计、现有Docker测试与本期/延期边界；所有实现细节/就近参考统一并入主设计对应章节。
- [x] D007 实现前把 UI 交互稿入口写入设计文档顶部/§31.7/§42.22，并声明实现须对照。
- [x] D006 两份专项设计完整合并主文对应章节，任务引用同步；旧文件仅作跳转索引。DDL字节不变，33表/70项文档与内存SQLite检查通过。

## M0：边界和原型（P0）

### T001 旧文件职责拆分
- [ ] 依赖：无；设计 §4、§7。
- 改动：AppState/AppCommand/AppEvent 与 storage/UI 超长文件先做机械拆分，保留 include 作用域和可见性；新功能落独立 ai 目录。
- 验收：结构改动不改变现有行为；cargo fmt/check 和原有相关测试；diff 中不夹带无关重构。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T002 建立领域 ID/DTO/错误和应用能力边界
- [ ] 依赖：T001；设计 §8–12、§20。
- 改动：core AI DTO；app Provider/Tool/Repository/Hook 契约；明确 SecretInput 的不可序列化与脱敏 Debug。
- 验收：core 不依赖 GPUI/HTTP/具体驱动；trait 对象安全；无新层级循环依赖；DTO 序列化白名单测试。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T003 模型基础库与流式 transport 原型
- [ ] 依赖：T002；设计 §6、§17。
- 改动：验证现有依赖、Rig 可步进循环/Provider 与自有策略的组合，优先复用可用基础能力；固定具体版本，选定 HTTP/SSE、cancel 和 schema 依赖。
- 验收：mock server 能返回分片/工具参数/usage/截断/错误；取消可中断等待；body/frame 有界；不引入外部 Agent 进程。
- 记录：所选 commit/版本、许可证、三端依赖和包体；若不采用 Rig，解释实测原因，而不是全部从零重写的口号。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T004 建立 scripted Provider 与隔离 fixture
- [ ] 依赖：T002；设计 §37。
- 改动：app tests/scripted Provider，可按调用接收真实 tool receipt 并继续；隔离 storage root、测试 SQLite/文件 fixture。
- 验收：不读取生产配置/真实 Key、不使用付费网络；脚本不绕过工具执行直接伪造数据库结果；正常 UI 命令能接入该 Provider。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T005 UI E2E 入口与截图预算工具
- [ ] 依赖：T004；设计 §38。
- 改动：测试专用启动/fixture 配置、每步语义断言和小图处理；先验证捕获/编码工具及平台可用性。
- 验收：raw 图不进入视觉上下文；≤1280×800、≤1MP、≤512KiB、目标≤250KiB；文字可读；每步 checkpoint；debug/test 注入不进生产入口。
- 测试方式：分别验证启动隔离应用、一次实际按钮点击/输入与状态断言、窗口捕获及首次读取前压缩；检查图片尺寸/字节/可读性和checkpoint，工具不可用明确受阻。


## M1：独立管理表、凭据与配置界面（P0）

### T006 首次创建 AI schema 与内置种子
- [x] 依赖：T002；设计 §35–36、§42。
- 改动：[schema.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/ai_platform/schema.rs)和init；SQLite open 的FK/busy timeout；不新增ai_schema_migrations/旧AI转换。
- 验收：首次建表、重复打开、事务中断、同名结构不匹配明确错误；DDL 唯一来源；不修改已有业务 kv/连接/history 内容。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T007 提取共享 AES/key loader 并兼容实际密钥字段
- [x] 依赖：T006；设计 §33。
- 改动：[secret_crypto.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/secret_crypto.rs)，连接后端转调用；兼容 secret_key/connection_secret_key、保留旧 AAD。
- 验收：现有连接密码全部可读；同名冲突显式失败；设置保存不丢密钥；新安装随机密钥原子初始化；旧内置钥匙不被偷偷替换。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T008 模型 private encrypted row 与凭据状态
- [x] 依赖：T007；设计 §33–34、§42.3。
- 改动：模型 ciphertext/nonce/cipher_version/key_id/secret_revision、AAD；公共 DTO 与私有行分离、Provider 专用 lease。
- 验收：不同模型交换密文/nonce/secret_revision 导致认证失败；12-byte nonce；同 Key 多次 Set 不复用 nonce；未知版本/损坏/错钥匙为 Unavailable；系统凭据调用数=0。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T009 模型 Keep/Set/Clear 与 CAS 原子保存
- [x] 依赖：T008；设计 §34。
- 改动：专用保存 API，同事务配置+秘密+revision；commit/磁盘错误注入。
- 验收：Keep 秘密列字节不变且不因密钥不可用阻断非敏感编辑；Set/Clear 正确递增；两编辑器冲突；失败保留旧 Key/配置；Debug/错误日志无敏感输入。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T010 模型/运行保护策略/运行设置独立 repository
- [x] 依赖：T006、T009；设计 §17、§30、§42.10。
- 改动：分页读取、逐对象 CRUD/CAS、能力/价格/endpoint/非敏感 JSON 校验；不新增 save_all_ai_config。
- 验收：未知价格不是免费；删除引用模型被阻止；禁用在下次调用生效；export 列白名单不含加密列；导入需补 Key。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T011 Agent直接提示词与运行快照
- [x] 依赖：T006；设计§19、§31.8、§42.10。
- 改动：ai_agents.system_prompt直接输入/保存/对象CAS，Turn捕获正文/hash与Agent revision；无模板/版本/发布表或页面。
- 验收：两Agent提示词互不覆盖；编辑只影响下个Turn；旧快照保持；UTF-8长度/空值安全基线；日志不输出全文，不因提示词放宽权限。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T012 Agent、插件 package/instance/binding 与 MCP 管理
- [x] 依赖：T010、T011；设计 §23–25、§42.10。
- 改动：逐实体save；内置Registry/system实例和绑定自动建立、default decision引用、system/user角色优先级；模型角色/MCP授权独立绑定；当前不支持第三方加载。
- 验收：每实体 revision 独立；失效引用/重复 binding/不兼容 schema 拒绝；Jev binding 不允许 required；无 Jev seed 可用。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T013 模型设置 UI
- [x] 依赖：T005、T009–T010；设计 §31–32。
- 改动：设置“大模型”菜单里的列表/编辑/测试连接、默认主/决策模型、Key占位、Keep/Set/Clear、loading、Conflict 提示；复用组件。
- 验收：E01；重启状态正确；不回显已存 Key；截图和测试日志无 Key；Esc/关闭/主题/键盘可达。
- 本任务功能/UI验收：原型 `#models`；从设置入口打开→搜索→新建/编辑→选择用途与Provider→校验非法输入→保存/取消→测试连接→切换默认主/决策模型→重启核对。表格六列、能力/上下文/计价/凭据状态、选择器和按钮对齐原型；验证Keep/Set/Clear、显式掩码切换及失败保留输入，不截图真实Key。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 状态校正：原勾选撤回；§K已记录未完成交互/视觉缺口，历史实现与测试记录保留，补齐本任务验收后再勾选。
- 验收记录（2026-10-05 第九轮，补齐后勾选）：
  - GPUI 交互回归：`cargo test -p fluxdb-desktop --offline ai_ui_` → **18 passed / 0 failed**（生产装配路径 `navicat_main_new`，含渲染快照有数据、搜索过滤与清空、新增只写一次、编辑不产生重复行、启停落库、默认模型选择、加载错误不显示为空态、完整 URL 与关闭取密、弹框内联校验与 CAS 冲突保留草稿、编辑态掩码 Key 预填且公开状态无明文、窄窗口渲染、列对齐、删除取消/Esc/遮罩与引用错误）。
  - 真实窗口 E2E（用户在 macOS 主窗口实际操作，结论无问题）：设置入口打开 → 搜索与清空 → 新建唯一测试模型 → 编辑名称/启停 → 非法输入报错且弹框不关闭、已填内容不丢 → Keep/Set/Clear 与显示/隐藏掩码 → 切换默认主模型与决策模型 → 取消/关闭/Esc/遮罩 → Tab/Shift+Tab 键盘导航 → 切换页面再返回 → **彻底退出进程后重新启动**核对记录保持 → 明暗主题与正常/窄窗口视觉。未截图真实 Key。
  - 未宣称项：真实远端模型调用成功响应本轮未验证（需真实 Key 与 M2 运行时），不作为本任务勾选依据。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [x] 预置已知模型，从设置“大模型”正常入口打开，生产渲染快照与表格均包含相同模型ID，名称/凭据状态/启用状态及过滤后的数量正确，不误显示空态。
  - [x] 实际新增并保存唯一测试模型，不重启、不手动刷新即出现对应行；编辑名称/启停后原行更新且不重复，页面与SQLite公开字段一致。
  - [x] 切换页面再返回及重启后记录仍可见；分别验证loading、真实空库、有数据和加载错误，错误不能被空列表掩盖，重试成功后恢复展示。
  - [x] 自动化断言穿过生产加载与AppState→渲染快照→视图路径，捕获非空状态被硬编码空集合覆盖；附新增后有数据的实机证据。

### T014 Agent直接配置/内置能力 UI
- [x] 依赖：T011–T013；设计 §31–32。
- 改动：设置“工具与MCP”菜单含工具/本地Server/内置插件/审计tabs；Agent主页面=左侧列表+右侧三分区（基本/模型与提示词/工具与内置能力）+底部保存栏（非弹框）；无权限/运行限制/目标/路由/费用字段；提示词直接输入，无Jev专用配置/版本下拉；失败保留草稿。
- 验收：界面修改实际对应独立表；CAS 冲突可见；无整体配置覆盖；加载损坏实体不自动重写空值。
- 本任务功能/UI验收：原型 `#agents` / `#tools` / `#plugins`；从侧栏进入Agent列表→新建→编辑三分区/提示词→绑定模型与工具→保存→切换Agent→重启核对；制造CAS冲突确认草稿保留。工具与MCP入口、四页签容器、工具列表、内置插件状态齐全，必经能力不可关闭；Server/审计内容分别由T060/T065实现，不做假可用按钮。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 状态校正：原勾选撤回；§K已记录未完成交互/视觉缺口，历史实现与测试记录保留，补齐本任务验收后再勾选。
- 验收记录（2026-10-05 第九轮，补齐后勾选）：
  - GPUI 交互回归：`cargo test -p fluxdb-desktop --offline ai_ui_` → **18 passed / 0 failed**（生产装配路径 `navicat_main_new`，含渲染快照有数据、搜索过滤与清空、新增只写一次、编辑不产生重复行、启停落库、默认模型选择、加载错误不显示为空态、完整 URL 与关闭取密、弹框内联校验与 CAS 冲突保留草稿、编辑态掩码 Key 预填且公开状态无明文、窄窗口渲染、列对齐、删除取消/Esc/遮罩与引用错误）。
  - 真实窗口 E2E（用户在 macOS 主窗口实际操作，结论无问题）：设置入口打开 → 搜索与清空 → 新建唯一测试模型 → 编辑名称/启停 → 非法输入报错且弹框不关闭、已填内容不丢 → Keep/Set/Clear 与显示/隐藏掩码 → 切换默认主模型与决策模型 → 取消/关闭/Esc/遮罩 → Tab/Shift+Tab 键盘导航 → 切换页面再返回 → **彻底退出进程后重新启动**核对记录保持 → 明暗主题与正常/窄窗口视觉。未截图真实 Key。
  - 未宣称项：真实远端模型调用成功响应本轮未验证（需真实 Key 与 M2 运行时），不作为本任务勾选依据。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [x] 预置两个不同Agent及插件绑定，从正常入口看见列表、选中详情与绑定；切换Agent字段不串，迟到加载不覆盖当前选中项。
  - [x] 通过界面新建/修改提示词与绑定、切换允许修改的插件开关，保存后列表与详情即时一致，返回/重启仍一致；失败保留草稿和原记录，不退化为空页。
- 本轮勾选（2026-10-07）：用户已在真实主窗口完成 Agent 编辑器逐项操作、与原型 `#agents`/`#tools`/`#plugins` 的视觉还原度核对及重启确认，补上此前缺口后勾选本任务。

## M2：主 Agent 和原生工具闭环（P0）

### T015 会话/轮次/消息/event 持久化
- [x] 依赖：T006、T012；设计 §9–12、§42.11。
- 改动：message header+parts、公开reasoning/plan/ref、UI state及panel preferences（mode/两宽/last session/新草稿）、稳定ID/序号/局部checkpoint、配置快照/活动Turn约束；正文单一真相不重复content_json。
- 验收：并发seq/part ordinal唯一、FK归属、250–500ms合并partial、迟到事件不污染新Turn；关闭重开/强杀恢复公开摘要/工具/审计/展开/滚动锚点；不持久化私有CoT，不每token写一行。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。
- 本轮勾选（2026-10-08）：补上此前缺口后闭环。进程级 SIGKILL 恢复测试（`sessions_sigkill_recovery_after_process_kill`，父进程拉起子进程写入后 kill，重开文件库断言公开摘要/审计/展开/滚动锚点完整恢复）通过；250–500ms merge partial 由运行时 300ms checkpoint（`conversation::checkpoint` 合并回同一 `-text`/`-summary` 行，终态强制保存）实现并验证，存储层 `repeated_checkpoints_update_one_message_and_parts_not_token_rows` 与 app `persisted_checkpoints_and_empty_terminal_apply_then_load_use_real_reply` 等通过。`fluxdb-storage --lib` 124 passed / 0 failed。


### T016 后台 actor、控制队列与 UI 事件桥
- [x] 依赖：T003、T015；设计 §12。
- 改动：内部 runtime 创建和生命周期；控制/事件 bounded channel，steering/cancel；不持 UI/SQLite 锁跨 await。
- 验收：主窗口可响应；取消事件不被大文本挤掉；关闭窗口任务安全结束；错误可见。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。
- 2026-10-08：在 `codex/ai-runtime-unify` worktree 收敛 T016/T028 生产链路，桌面发送/停止/审批恢复统一接入 actor，保留会话事实持久化与投影；新增运行中 steering、排队取消、终态背压与真实 SQLite 审批恢复验证。真实窗口因已有实例的单实例锁及 GUI 自动化服务不可用而受阻，保持未勾选；实现、验证及退出时限边界见 [本轮记录](testing/ai-agent/t016-runtime-unification-2026-10-08.md)。
- 2026-10-08（下午）：补上真实窗口验收。使用 `FLUXDB_PROFILE=t016accept` + 隔离 `FLUXDB_AI_TEST_ROOT` 绕开默认实例单实例锁，配合 RFC 5737 不可路由端点 fixture 与离线 `burst`/`error` scripted 模型，在真实 macOS 窗口验证：主窗口可响应、`SIGTERM` 优雅关闭后 SQLite `integrity_check=ok` 且 10 条 Turn 全部 `completed`、error 夹具在 UI 显示红色错误卡片并在 SQLite 记录 `failed`+`error_code`。burst 真实窗口单次 Turn 仅 ~140ms，手动点击停止无法落在窗口内，取消语义由既有自动化（运行时控制 7 项 + GPUI 面板 63 项中的 stop 用例）等价覆盖。自动化基线 `cargo fmt --check`、`cargo check -p fluxdb-desktop --locked --all-targets`、`cargo test -p fluxdb-app --locked --lib ai_runtime`、`cargo test -p fluxdb-app --locked --test ai_runtime_conversation`、`cargo test -p fluxdb-desktop --locked ai_panel` 全部通过。证据与差异说明见 [真实窗口验收补充记录](testing/ai-agent/t016-real-window-2026-10-08.md)。


### T017 主模型适配与秘密 transport 边界
- [x] 依赖：T003、T008、T016；设计 §8、§17。
- 改动：generation Provider 流、模型能力和工具名称映射、请求前 credential lease、usage。
- 验收：调试默认 scripted，真实模型调用要等 T030 计量与执行保护就绪；模型请求/日志/快照无 Key；HTTP 默认跨域 redirect 不带认证；分片/断流/429/截断测试；版本与 capabilities 匹配。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T018 主循环与工具调用状态机
- [x] 依赖：T017；设计 §10–11。
- 改动：prepare→stream→校验工具→执行→观察→下一轮；安全边界吸收用户补充。
- 验收：真实工具错误进入下一轮；输出截断不执行；同参数无进展可停止；不是固定意图路由；无 Jev 完整运行。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T019 工具 registry、schema、发现与结果协议
- [x] 依赖：T018；设计 §20。
- 改动：ToolDescriptor/Receipt、registry search/describe、Provider名称映射、结果分页/截断。
- 验收：未知工具/错误 schema 返回受控错误；发现不授权；input/output 大小限制；receipt 与模型/UI projection 分离。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T020 目标授权、风险策略与审批表+询问AI内联批准卡片
- [x] 依赖：T019；设计 §25、§29、§42.11。
- 改动：内核权限交集、最终plan hash、强制SecurityAuditHook与append-only审计表、scope/version、审批暂停/决定；denied 与 unknown 不误作可重试。
- 验收：参数/目标/plugin digest 改动审批失效；撤销即时生效；deny优先；批准不绕过数据库权限；卡片状态回显与刷新后仍可处理；无全局永久允许开关。
- 本任务功能/UI验收：原型 询问AI审批卡；实际点击拒绝确认数据不变，再发起独立请求批准并核对后端变化；修改参数/目标后旧卡不可批准，重复点击不重复执行。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。

#### T020 验收记录（2026-10-09）

变更：审批卡补齐头部/风险徽标、连接上下文、可复制等宽计划、影响预估与依据、约束清单、Esc/遮罩提示（`main_parts/ai/approval.rs`）；批准改为后台提交 + 提交中禁用 + 重复点击防护（`ai/actions.rs`、`ai/state.rs`）；Esc 与关闭面板走真实拒绝链路；`show_message` 承载决定失败与恢复失败（`data_table_ui/message_status.rs`）；`ApprovalDetails` 只展示内核已核实事实，未核实行数显示"未知"（`ai_permission.rs`）。

平台：macOS arm64（真实窗口 E2E + Headless UI）。Windows/Linux 未验证。

已运行：

```sh
cargo fmt --all -- --check
cargo check --locked
cargo test --workspace --locked            # 2170 passed / 23 ignored
cargo test -p fluxdb-desktop --locked ai_panel_   # 69 passed
cargo test -p fluxdb-app --locked ai_permission   # 15 passed
```

真实窗口 E2E（隔离 `FLUXDB_PROFILE=t020verify` + `FLUXDB_AI_TEST_ROOT`，Swift/CGEvent 无法编译，改用 clang + Accessibility API 驱动）：

| 场景 | 操作 | 后端核对 | 截图 |
|---|---|---|---|
| 待审批卡 | 打开面板 | `ai_approvals.status=pending, rev=1`；工具调用 `waiting_approval` 且无回执 | `shots/01-card-light.jpg`、`04-card-dark.jpg`、`05-card-narrow.jpg` |
| 遮罩 = 拒绝 | 点击面板外遮罩 | `status=denied, rev=2`；重新打开面板后卡片消失 | — |
| 批准一次 | 点击"批准一次" | `status=approved, rev=2`；卡片原地变为"审批决定，已批准一次"；工具调用仍 `waiting_approval`（未重复执行） | `shots/03-approved-and-error.jpg` |
| 恢复失败提示 | 同上 | 窗口底部居中显示"无法恢复任务：任务快照缺少原模型，拒绝改用当前模型" | 同上 |
| 重复点击 | 连点两次"批准一次" | `rev` 只递增一次，`status=approved`，无第二次决定 | — |

视觉：明暗主题卡片各区块完整；窄窗（820×700）下 SQL 换行、影响预估两列并排、按钮不溢出。

证据：`/private/tmp/fluxdb-t020-e2e/shots/`（压缩 JPEG 小图）。

未验证：Windows/Linux 真实窗口；真实模型与真实数据库写入（fixture 模型端点为 RFC 5737 不可路由地址，恢复必然失败，故批准路径的后端证据止于"决定已提交且未重复执行"，真实写入由 `ai_conversation_write_tool` 的真实 SQLite 集成测试覆盖）。


### T021 自动捕获当前界面并固定执行 scope（无用户确认）
- [x] 依赖：T016、T020；设计 §13。
- 改动：UI capture 小型 DTO，Turn target pin、显式 retarget/缓存失效；不打包连接敏感配置。
- 验收：E04；切标签不改 SQL/Redis 目标；retarget 使旧批准失效；无目标时解释任务仍可进行。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T022 `@` 引用、候选检索与附件 UI
- [x] 依赖：T014、T021；设计 §14、§31。
- 改动：ContextRef/chip、多对象候选、失效/同名处理、文件选择与授权。
- 验收：E09；只传显式引用/摘要，详情按需；@ 不给写权限；键盘选择、Esc、外部点击正确。
- 本任务功能/UI验收：原型 询问AI的`@`与附件；输入@→筛选同名候选→键盘/鼠标选择→显示chip→移除；无匹配、失效对象、非法文件可见反馈，Esc/外部点击关闭且不穿透。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [x] 预置同名但不同ID/目标的候选，检索实际显示正确结果；选择后chip对应正确引用，移除后提交内容不再包含该引用；区分无匹配与检索失败。
- 验收记录（2026-10-09 补齐后勾选，平台 macOS arm64）：
  - 改动文件：`crates/fluxdb-app/src/parts/ai_references.rs`、新增 `crates/fluxdb-app/src/parts/ai_reference_details.rs`（`context.read` 详情、引用校验/授权）、`crates/fluxdb-app/src/parts/ai_conversation.rs`（运行快照 `reference_grants`）、`ai_conversation_context.rs`（引用不授予写权限提示）、`ai_conversation_runtime.rs`、`ai_loop.rs`/`ai_conversation_execution.rs`（`TurnState` 透传 `session_id/turn_id`）、`apps/fluxdb-desktop/src/main_parts/ai/{references,composer,message,state,actions,panel}.rs`；夹具 `crates/fluxdb-app/examples/ai_reference_fixture.rs`、mock 模型 `docs/testing/ai-agent/t022_mock_model.py`。
  - 候选类型补齐：连接、库/schema、表/视图、列/索引/集合、Redis DB/Key、知识条目 + 保存查询、编辑器选区、结果页快照、已授权附件；上限 20；同名按连接+库+schema 全路径与 ID 区分。
  - 无匹配与检索失败分离：候选检索返回 `Result`，`ai_reference_errors` 汇总逐项失败，UI 显示「检索失败：…」与「无匹配引用：@…」互斥。
  - 失效对象：chip 前跑内存/存储校验，失效项红框 + 「失效」标记 + tooltip；跨连接/库/schema 合并冲突提前标记。
  - 详情按需：`context.read` 工具按 opaque ref 读取保存查询/选区/结果页有界快照，限定本轮 `session_id/turn_id`；结果页列 32/行 10/单元格 128 字符，二进制只记字节数。
  - 只传摘要：第 1 次模型请求的 `context_references` 仅含 `{id,label,kind,summary,target}`，不含正文/路径；正文经 `context.read` 在第 2 次请求到达。
  - `@` 不给写权限：system 提示明确只读定位、写仍需审批；只读连接 Deny、可写连接 `waiting_approval`，未批准真实工具调用数为 0。
  - 交互收口：键盘上下键/回车选择、Esc 只关浮层不关面板、外部点击关闭且不穿透、`@` 按词边界识别。
  - GPUI 交互回归：`cargo test -p fluxdb-desktop reference` → 20 passed / 0 failed；App 集成 `ai_reference_send` 14、`ai_session_model_selection` 23、`ai_restart_recovery` 5、`ai_conversation_write_tool` 11 全通过；`cargo fmt --all -- --check`、`git diff --check`、`cargo check --locked --offline --workspace` 通过。
  - 真实窗口 E2E（引用→模型→工具→回执→结果）：本地 mock OpenAI Chat Completions（不联网）+ 隔离夹具，900x700 可见窗口点击输入框→`@订单`→选中「保存查询：订单口径」→发送；mock 收到 3 次请求（主生成/带工具回执/标题），`ai_turns.status=completed`，`ai_tool_calls=context.read/succeeded`，助手正文含真实回执。证据：`/private/tmp/fluxdb-t022-shots/{e2e-chip,e2e-prompt,e2e-result}.png`、`e2e-mock-model.log`。
  - 视觉：明暗主题/窄窗（900x700）与正常宽（1440x900）截图已产出（`dark900-dropdown.png`、`light900-nomatch.png`、`wide-dropdown.png` 等）；原型还原度由用户对照 `docs/prototype/ai-agent-ui.html` 完成。
  - 未验证：Windows/Linux（按 B 节平台分工由用户自行验收；附件安全读取在非 Unix 明确不可用）；原生系统文件选择对话框点选、IME/无障碍映射；真实第三方模型与真实 PG/MySQL 数据库。完整记录见 `docs/testing/ai-agent/t022-references-2026-10-09.md`。

### T023 元数据探索和显式 SQL 执行服务
- [x] 依赖：T019–T021；设计 §4、§20–21。
- 改动：复用 Connector；从既有 scope query 方法提取独立服务，明确 QuerySessionId、history 与 source；不构造 GUI tab 执行后台任务。
- 验收：搜索/描述/查询真实测试库；取消/进度正确；Agent 与编辑器临时表/事务不串；既有查询回归通过。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。
- 验收记录（2026-10-09，macOS arm64）：真实 PostgreSQL 17 + MySQL 8（podman 隔离容器）跑通 `t023_*` 真库用例（搜索/描述/查询/取消/进度/会话隔离）；修复 AI 会话与编辑器标签撞号、AI 会话不释放、`truncated` 不反映主动 LIMIT 三个缺陷；`cargo test --workspace --locked` 通过。完整证据见 `docs/testing/ai-agent/t023-2026-10-09.md`。未验证：Windows/Linux、真实桌面窗口、真实第三方模型 tool calling。


### T024 SQL 只读保证、风险分类和修改执行
- 2026-10-06（B）：参数绑定实现 MySQL 真 bind + tagged 参数校验（PG/SQLite 仍明确拒绝）；真实 MySQL 验收见 `docs/testing/ai-agent/t024-param-binding-2026-10-06.md`。当时 PG/SQLite 绑定与占位符方言转换未完成，因此保持未勾选。
- 2026-10-06：② 注册写工具 `db.execute`（写权限取固定目标连接只读标记 + 逐次审批；计划哈希改为规范化 JSON；拒绝补事实行）已完成并有 app 集成与真实窗口验收记录：`docs/testing/ai-agent/t024-write-tool-2026-10-06.md`；真实 MySQL/PostgreSQL 写入属 ③，未做，保持未勾选。
- [x] 依赖：T023；设计 §21。
- 改动：对应 dialect AST、服务端读取策略、db.query/db.execute/db.explain、批量计划/审批。
- 验收：WITH 修改、SELECT INTO、副作用函数、EXPLAIN ANALYZE、锁语句、未知语法、跨 schema；E05；不能保证的数据库明确降为需授权风险，不宣称安全读取。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。
- 验收记录（2026-10-10，macOS arm64）：WITH 修改、SELECT INTO、副作用函数、未知函数、EXPLAIN ANALYZE、锁语句、未知语法逐项分类并拒绝自动只读；新增 PG/MySQL/SQLite 作用域校验，跨 schema/database/attached database 和越界函数执行前 `scope_mismatch`。E05 通过生产链路验证拒绝不变、载荷漂移失效、批准只执行一次；真实 PG/MySQL 写入回执、回读和副作用计数一致。PG/SQLite 参数仍明确 `parameters_unsupported`，不插值、不宣称安全自动读取。`cargo test --workspace --locked --offline` 通过；完整记录见 `docs/testing/ai-agent/t024-close-2026-10-10.md`。未验证：Windows/Linux、真实桌面窗口本轮回归（既有真实窗口写审批证据见 2026-10-06 记录）。


- 2026-10-06（A）：真实模型 + 真实 MySQL 端到端（含 parameters 绑定）通过，见 `docs/testing/ai-agent/t024-live-mysql-e2e-2026-10-06.md`。

### T025 Redis 探索与命令执行工具
- [x] 依赖：T019–T021；设计 §22。
- 改动：SCAN/inspect 范围工具、命令/子命令/参数风险、Cluster/DB scope、长订阅停止。
- 验收：E08；大集合不无限加载；COUNT 非硬上限测试；Lua/FLUSH/ACL/CONFIG 风险；Cluster 作用域和取消反馈真实。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。
- 2026-10-10：长订阅停止已实现——新增 `redis.subscribe` 有界工具（限时/限条数/限字节/可取消），`redis.execute` 仍拒绝订阅命令并指向专用工具；真实 Redis 7.2.7 自动化 7 个用例通过，真实桌面窗口手动验收通过（自然语言触发，条数上限按预期收工）。
- 2026-10-10：命令参数授权已实现——按命令 key spec 解析 argv 全部 key（单/多/成对/无 key），脚本、跨节点/跨库搬移与未知命令 fail closed；`RedisKeyScope` 空 prefix 不等于全 Key；`redis.execute`/`inspect` 发送前校验，`redis.scan` 按授权 prefix 过滤（pattern 不是授权）；授权数据来源（profile `all_keys`/`key_prefixes`）属 T057。真实 Redis 越权写入被拒且服务端无写入，11 个 live 用例 + 10 个 key spec 单测通过。
- Cluster 作用域随集群连接能力整体延后（Redis Cluster 的产品级连接当前未投入使用，SCAN 也无法在无真实集群的前提下验证节点级 opaque cursor；相关字段已如实标注 `cluster_complete: false` / 单节点范围）；用户于 2026-10-10 手动验收全部通过后勾选本条。详见 `docs/testing/ai-agent/t025-2026-10-10.md`。


### T026 文件搜索/读取与隐私边界
- [x] 2026-10-10（macOS arm64 收口）：Path-based grant、canonical/symlink 安全打开、CSV/TSV 流式记录分段、文本按字节边界截断、二进制类型/大小/预览反馈、凭据与越权 symlink 拒绝、E09 headless 与真实窗口验收全部完成。**真实窗口验收**：用户在隔离 profile（`FLUXDB_PROFILE=t026-e09`）下人工完成 6 场景轮（合法 CSV chip/摘要、大文件拒绝、凭据名拒绝、symlink 拒绝、plain.sql 对照授权、chip 移除），全部通过。**headless**：fluxdb-desktop 601/601；fluxdb-app 740/740 + 集成全绿。**仍未验证**：Windows/Linux 真机编译与桌面行为（代码 cfg 隔离，待用户自行按平台跑）。完整记录：`docs/testing/ai-agent/t026-e09-2026-10-10.md`。
- [ ] 依赖：T019–T022；设计 §14、§29。
- 改动：Path-based grant、canonical/symlink 安全打开、文本/CSV 分段、二进制反馈；禁止凭据文件读取。
- 验收：E09；路径授权、symlink/编码等在当前开发平台测试，并保持各平台路径语义兼容；不能绕过目录授权读取 config.toml/sqlite 凭据。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。
- [x] 2026-10-10（macOS arm64）：**CSV 分段与二进制反馈已实现**。新增 `crates/fluxdb-app/src/parts/ai_file_content.rs`：CSV/TSV 走 `csv` crate 流式记录分段（引号转义与嵌入换行不按字节切），返回 `records/record_count/next_offset/truncated`；文本按字节分段但在 UTF-8 字符边界安全截断（切到半个字符回退并如实上报）；授权阶段取文件头 8 KiB 判定内容类型，`binary` 标记写入宿主 source（不进模型上下文），`file.read` 对二进制只返回 `content_type/size/preview` 不回传正文。`file.read` schema 新增 `max_records`（1–256，超限在 prepare 阶段拒绝）。
- 修复的真实缺陷：① 二进制判定原先用「当前读取段」判定，offset 落在多字节字符中间时会把合法文本误判为二进制——改为以授权时头部取样结果 `grant.binary` 为准；② CSV `truncated` 原按字节比较 `consumed < total_len`，末条记录后解析器位置可能仍小于总长而误报「已读完」漏数据——改为预读一条判断是否真有剩余记录；③ CSV `next_offset` 曾因多读一条记录被推过头而跳过数据记录，已回退为 `reader.position()` 语义。
- 本轮命令（macOS arm64）：`cargo fmt --all -- --check` 通过；`cargo check --workspace --locked --offline` 通过；`cargo test -p fluxdb-app --locked --offline` → lib 740 passed / 0 failed / 1 ignored，集成 24/4/10/2/9/2/6/5/4/14/11/6/28/7 全通过。`file_references` 用例 11 → 16，新增 CSV 记录分段/分页续读、CSV 未闭合引号按数据处理、记录数上限、文本多字节边界、二进制内容类型反馈与授权摘要不含路径。
- 2026-10-10：Windows 安全读取已实现（原为「暂不可用」）。`open_ai_authorized_file` 逐级 `CreateFileW` + `FILE_FLAG_OPEN_REPARSE_POINT`，逐级按句柄属性拒绝 reparse point（junction/symlink），末端以 `GENERIC_READ` 打开并用 `GetFinalPathNameByHandleW(VOLUME_NAME_DOS)` 核对真实落点与授权路径一致。Win32 无 `openat`、无法把父句柄传给下一级，故用「逐级校验 + 末端真实路径核对」覆盖下钻途中被换链的竞态。`crates/fluxdb-app/tests/ai_session_model_selection.rs` 的 Windows 用例从「断言不可用」改为「读回真实内容」+「末端/中间 reparse point 替换均拒绝」。
- 未验证：Windows 上仅完成 `cargo check --target x86_64-pc-windows-msvc` 的类型检查（用独立 crate 对 `windows 0.57` 实际 API 编译验证），**未在真实 Windows 机器上编译、运行或做窗口验收**；junction 与 symlink 拒绝行为、真实句柄读取均为待平台验证。Linux 走 `#[cfg(unix)]` 既有实现，本轮未改，仍待平台验收。Windows 上 `CreateFileW` 需 `Win32_Security` feature（windows 0.57 源码确认门控关系），`fluxdb-app` 已按 target 条件依赖，不影响 macOS/Linux 构建。E09 端到端脚本与真实窗口验收尚未执行。
- 2026-10-10：Windows 安全读取已实现（原为「暂不可用」）。`open_ai_authorized_file` 逐级 `CreateFileW` + `FILE_FLAG_OPEN_REPARSE_POINT`，逐级按句柄属性拒绝 reparse point（junction/symlink），末端以 `GENERIC_READ` 打开并用 `GetFinalPathNameByHandleW(VOLUME_NAME_DOS)` 核对真实落点与授权路径一致。Win32 无 `openat`、无法把父句柄传给下一级，故用「逐级校验 + 末端真实路径核对」覆盖下钻途中被换链的竞态。`crates/fluxdb-app/tests/ai_session_model_selection.rs` 的 Windows 用例从「断言不可用」改为「读回真实内容」+「末端/中间 reparse point 替换均拒绝」。
- 未验证：Windows 上仅完成 `cargo check --target x86_64-pc-windows-msvc` 的类型检查（用独立 crate 对 `windows 0.57` 实际 API 编译验证），**未在真实 Windows 机器上编译、运行或做窗口验收**；junction 与 symlink 拒绝行为、真实句柄读取均为待平台验证。Linux 走 `#[cfg(unix)]` 既有实现，本轮未改，仍待平台验收。Windows 上 `CreateFileW` 需 `Win32_Security` feature（windows 0.57 源码确认门控关系），`fluxdb-app` 已按 target 条件依赖，不影响 macOS/Linux 构建。


### T027 结果句柄、分页与保留管理
- [x] 2026-10-10（macOS arm64）：**已实现**。`ResultStore` 按 session/turn 绑定归属，内存预算外写受控根 `ai-results/results/<uuid>.json`；`SqlExecutionOutcome::to_model_json` 回执只带 5 行样本 + `row_count`/`rows_omitted` + `result_ref`，完整行走仓库；新增 `result.read_page`（`ai_result_pages.rs`，只读快照不重跑来源工具）；`insert_result` 接 `ResultRetention` 支持 retained 落盘（相对路径/字节数/内容哈希入库，越界路径拒绝）；重启恢复过期 memory 与到期的 retained，并删除过期 spill 文件。
- 修复的真实缺陷：① `read_page` 原只返回内存部分却给出「还有更多」游标，spill 条目永远读不到；改为按需回读文件，缺失/损坏/条目数不符报 `result_damaged`；② `standard_sql_catalog_with_results` 注册了 `result.read_page` 实现却没加 spec，`ToolCatalog` 拒绝注册；③ `ai_results.retention_kind` 固定 `'memory'`，溢写文件重启后无归属；④ 回执登记靠调用方另传一份结果仓库，而工具目录已持有同一个仓库，两处各传一次迟早漏传（直接调 `run_ai_conversation_turn` 的路径就会把溢写结果登记成 `memory`），改为 `ToolCatalog::with_results` 由目录持有。
- 端到端：新增 `crates/fluxdb-app/tests/ai_result_paging_e2e.rs`，真实 SQLite 文件 + 真实驱动（demo MockConnector 只返回 2 行、触发不了 spill）+ 生产装配链，覆盖「大结果不进上下文 → retained 落点可核对 → `result.read_page` 分页读回不重不漏 → 跨会话读不到 → 重启后 retained 仍在」。
- 本轮命令（macOS arm64）：`cargo fmt --all -- --check`、`cargo check --workspace --locked --offline --all-targets` 通过；`cargo test --workspace --locked --offline` 全部通过（fluxdb-app lib 761 passed / 0 failed / 1 ignored，集成 24/4/10/2/9/2/6/5/4/14/12/1/6/28/7 passed）。验收记录见 [docs/testing/ai-agent/t027-2026-10-10.md](testing/ai-agent/t027-2026-10-10.md)。
- 未验证：Windows / Linux 未编译与原生验收（相关代码无平台专属分支，仍待三端各跑一次）；不含 UI，未做真实窗口 E2E（结果卡在 T028 范围渲染，`result.read_page` 的面板分页入口仍未交付，见 T028 验收边界）；设计 §42.7 的「用户可配置保留策略与空间上限」未实现，当前只落地 TTL 过期 + 路径/hash 可核对 + 过期文件清理。
- 依赖：T023–T026；设计 §15、§42.7。
- 改动：ResultRef、memory/retained/expired、spill 配额、source 归属，result.read_page。
- 验收：大结果不全进上下文；过期/重启内存结果有明确错误；retained 的路径/hash/清理可验证。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T028 AI 对话面板/时间线/审批/产物容器
- [x] 依赖：T018、T020、T022、T027；设计 §31。
- 2026-10-10（macOS，用户手动验收确认通过）：按 §39.4 六项门槛（功能完整性、数据展示闭环、交互回归、真实窗口 E2E、视觉还原度、修复闭环）逐项人工验收通过。
- 勾选记录：[T028验收记录（2026-10-10，用户手动验收）](testing/ai-agent/t028-2026-10-10.md)。既有自动化/真实窗口证据：`cargo test -p fluxdb-desktop --locked ai_panel_`（面板、审批、恢复、布局、发送/停止、明暗）；上游闭环见 `docs/testing/ai-agent/t020-t029-2026-10-06.md`、`t022-references-2026-10-09.md`、`t026-e09-2026-10-10.md`、`t027-2026-10-10.md`、`t028-2026-10-06.md`。
- 验收边界（如实记录，经用户确认接受当前范围）：产物 `artifact_ref` 按持久化事实渲染为任务产物容器，报告弹框/图表/导出属 T040；结果引用卡展示真实句柄与状态，`result.read_page` 的面板分页入口仍可后补；历史入口当前为最近排序列表，分组/重命名/归档在设置页会话管理，历史分页续读未接。
- 改动：原生 composer、流式文字/工具状态、发送/停止、等待审批、loading 和结果引用。
- 验收：E02–E05、E09、E13；真实窗口启动；普通控件复用组件、颜色主题、图标、弹层防穿透；无需 Jev。
- 本任务功能/UI验收：原型 `#workbench`询问AI / `#history`；交付抽屉/内嵌切换、默认模式、两宽记忆与60%/40%限制、新建/关闭、历史入口、公开摘要/计划/工具卡及产物容器。发送→流式→工具→结果；停止、折叠、上滚不抢位置；U07/U08验证窄窗口/高DPI/会话草稿/焦点，关闭面板不停止或重放任务；不每token建卡。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [x] 预置带消息/工具/产物的会话，从历史入口打开，实际卡片字段/顺序对应持久化事实；空会话与加载失败有不同显示。
  - [x] 真实发送后用户消息、流式内容、工具及最终结果经生产事件桥与快照即时可见，不靠刷新；切会话后旧事件不串入当前会话。

### T029 停止、重启和未知副作用恢复
- 2026-10-06：① 启动扫描/重新审批/未知写防重放已实现，存储与 GPUI 回归、隔离真实窗口分步验收见 `docs/testing/ai-agent/t029-restart-2026-10-06.md`；② 写工具注册与③ 真实模型/Docker 验收本轮不实施。完整 T029 保持未勾选。
- 2026-10-10：② 写工具 `db.execute` 已由 T024 注册；③ 真实 PostgreSQL + 触发器副作用计数 + 真实窗口 E2E 完成，见 `docs/testing/ai-agent/t029-2026-10-10.md`。E11 四步通过，副作用计数证明没有重复执行；停止路径由存储/GPUI 回归覆盖，Windows/Linux 未验证。
- [x] 依赖：T015–T028；设计 §11、§21。
- 改动：启动扫描 interrupted/running calls；重新核实、重新审批、禁止未知写重放；关闭窗口取消。
- 验收：E11；故障注入 prepared→执行→回执各窗口；读取消和不可取消写分清；副作用调用计数证明没有重复执行。
- 本任务功能/UI验收：原型 询问AI / `#history`；运行与等待审批时退出/故障重启→恢复同一会话、草稿、锚点及卡片；unknown写状态可见且不自动重放，核对后端调用数。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [x] 正常退出及故障重启后，从历史列表进入已知会话，实际消息/工具/审批/unknown状态与存储事实一致；核对可见记录和后端调用数，不能只验证恢复查询返回数据。

### T030 主循环 Token 计量与执行保护
- 2026-10-11：Token 口径实现与本机统计表重建完成；串行工作区、存储/App/协议与明暗 Headless 通过；**真实窗口由用户手动验收确认通过**（代理侧桌面自动化不可用，未产出截图，门槛以用户确认为准）。记录：[T030 Token 验收](testing/ai-agent/t030-tokens-2026-10-11.md)。
- [x] 依赖：T010、T017；设计 §30、§42.8。
- 改动：SQLite 短事务登记/完成请求；按 session 统计 input/output/cached/total tokens；缺 usage 明确 unknown，保留迭代/时间/并发等执行保护。删除金额限制、费用计算、价格快照和费用预留表；模型价格配置保留供后续路由，不影响计量或放行。
- 验收：E12；登记/完成幂等且并发不超占名额；缓存不重复计入总量；未知 usage 不当零用量；价格缺失/变化不影响实际 token；切会话隔离、新草稿归零、重启恢复；不设置金额或累计 token 限额。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T031 无 Jev 核心 E2E 门槛
- [x] 依赖：T005、T028–T030；设计§37–39。必经审计不在本任务范围，由 T065/T066 单独验收（2026-10-11 用户确认，见文末修订）。
- 2026-10-11（macOS arm64，用户手动验收确认通过）：用户确认已在真实主窗口与实际使用中逐项通过 E01–E05/E08/E09/E11/E13，门槛以用户确认为准（沿用 T028/T030 先例）。**未产出**串行 E2E 脚本、每步 checkpoint/小图与 decision 计数断言；T005 仍未勾选。记录：[T031 勾选记录](testing/ai-agent/t031-2026-10-11.md)。
- 改动：串行 E01–E05/E08/E09/E11/E13 自动化脚本与后端断言（不含审计用例）。
- 验收：数据库/文件实际结果、UI 小图、每步 checkpoint、decision 请求数=0；不能只检查聊天文案。
- 测试方式：执行本任务列出的E/U用例，scripted与真实请求分开记录；真实主模型用“公司自建”，涉及Jev时用“决策模型”（设计§37.3），核对实际工具/后端结果和窗口截图，不以聊天结论代替断言。


## M3：可选决策、上下文、Hook 与 BI（P1，安全边界仍 P0）

### T034 上下文装配与截断续读（2026-10-11，本轮修复大结果投影）
- [x] **修复真实缺陷**：`project_receipt` 原来按字节硬切 `result_json`，而 `ai_loop` 把该片段原样嵌进 `{"tool":..,"truncated":..,"result":<投影>}` 信封发给模型。回执超过工具 `output_limit_bytes` 时会截在 JSON 字符串/数组中间，产出**模型无法解析的半截 JSON**，模型看到的是坏内容而不是"结果被截断"这一事实。已用失败测试先复现（`EOF while parsing a string`）。
- [x] 修复后投影恒为合法 JSON：JSON 对象回执改为在字节预算内**逐步丢弃顶层字段**（从末尾向前，`rows` 这类大字段优先省略），并写入 `omitted_fields` 让模型知道内容是被省略而非不存在；非 JSON 文本回执按 UTF-8 边界截断后作为合法 JSON 字符串返回；预算小到装不下完整说明时退到 `{error:result_too_large}`，0–1 字节预算返回空串——工具声明的 output 上限是硬约束。
- [x] 测试：`crates/fluxdb-app/src/parts/tests/ai_tool_registry.rs` 新增 3 项（截断后信封合法 JSON；截断保留 `columns`/`row_count` 并标注 `omitted_fields` 且不超上限；非 JSON 回执在多个小预算下均合法且有界），`ai_tool_registry` 共 17/17 通过。既有 `receipt_projection_is_bounded_and_immutable` 与 `projection_honors_spec_output_limit` 两条断言旧截断行为的用例无需改动即通过（新契约是旧断言的超集：仍有界、仍标记 truncated、仍不改原始回执）。
- 验证：`cargo fmt --all -- --check` 通过；`cargo test -p fluxdb-app --locked --offline` → **764 passed / 11 failed**（较上一轮 761 增加 3 项新测试），11 项失败仍全部为沙箱禁止 `TcpListener::bind("127.0.0.1:0")` 的既有用例，与本次改动无关；`ai_loop` 18/18、`ai_conversation_tools`/`ai_conversation_write_tool`/`ai_result_paging_e2e` 全部通过。
- 未完成（仍不足以勾选 T034）：`editor.read_range` 工具尚不存在，`ai_context.rs` 的截断标记已宣称"可用 editor.read_range 读取"但该工具未实现——模型被告知可以续读却无法调用，是当前真实缺口；界面摘录 `16 KiB / 400 行` 限额仍是固定常量，未按最终模型窗口调整；图片 decoded 像素/活动数量限制当前无任何图片进模型路径，尚未产生实际约束；ResultRef 媒体引用、路由事件落库、原型对照截图仍未做。

### T034 补充：工具输出形状契约（2026-10-11，对齐 Pi/Goose 做法）
- [x] 核对本地参考实现后调整设计：Pi（`packages/durable/src/truncate.ts`、`harness/output.ts`）按工具声明的 `outputLimits.retain` 保留 head/tail 且**从不返回半行**；Goose（`crates/goose/src/agents/large_response_handler.rs`）超 200k 字符直接落盘、回给模型文件路径而非截断内容。共同点是**截断发生在产出侧**，传输层只兜底。
- [x] 新增 `ToolOutputShape` 契约（`crates/fluxdb-app/src/parts/ai_tool_registry.rs`）：`Sample { keep_rows, omit_field, rows_omitted_field }` 表示工具产出时已自行裁剪（样本行 + 省略行数 + ResultRef），`Opaque` 表示内容本身是产物（文件读取、Redis inspect），不做结构猜测。`ToolSpec` 增加该字段，`from_descriptor` 默认 `Opaque`。
- [x] `ToolCatalog::project` 按声明形状投影：`shrink_to_budget` 的丢弃顺序以声明字段优先，其余仍从末尾向前；`too_large_marker` 对 `Sample` 形状提示"用 result.read_page 分页读取"，对 `Opaque` 提示缩小读取范围。`db.query`/`db.explain` 在 `standard_sql_tool_specs` 中声明 `Sample{keep_rows:5, omit_field:"rows"}`。
- [x] 同时修正一处根因：`SQL_RESULT_MAX_BYTES`（128 KiB）此前与工具投影上限相等，导致 `db.query` 结果先被自身上限截断、再被投影层截第二次——正是坏 JSON 的来源。现投影对未超限载荷原样透传（新增断言），不再二次裁剪。
- [x] 测试：`ai_tool_registry` 19/19（新增 `Sample` 形状按声明顺序优先丢 rows、保留 columns/row_count/rows_omitted；`Opaque` 不发明结构），`ai_sql_exec` 22/22（含新增 `db.query` 声明 `Sample` 且未超限时原样透传的用例）。既有投影断言全部无需改动。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked --offline` 通过；`cargo test -p fluxdb-app --locked --offline` → **767 passed / 11 failed**（较上一轮 764 增加 3 项），11 项仍为沙箱禁止本地监听的既有 mock server 用例。
- [x] 其余工具显式声明形状（契约真正被使用，不再只靠默认值）：新增 `add_tool_with_shape`，形状由注册处给出而非通用层从 id/描述反推。`file.read`/`file.search` 与 `context.read` 在注册处声明 `Opaque`（附件正文、CSV 记录、保存查询/选区快照内容本身就是产物）；`standard_redis_tool_specs` 五个工具显式声明 `Opaque`（scan 游标分页、inspect 限值预览、subscribe 限时限条数字节，均已在产出侧有界）；`result.read_page` 声明 `Sample{omit_field:"items"}`——这一项顺带修掉一个真实问题：它此前没有可丢弃的大字段，超限时只能整体退化为 `result_too_large`，模型拿不到 `next_cursor` 就无法继续翻页；现在丢 `items` 而保留 `result_ref`/`next_cursor`/`total_items`。
- [x] 测试：`ai_result_pages` 6/6，新增用例锁定 `read_page` 裁剪后仍保留翻页游标。
- 验证：`cargo fmt --all -- --check` 通过；`cargo test -p fluxdb-app --locked --offline` → **768 passed / 11 failed**（较上一轮 767 增加 1 项），11 项仍为沙箱禁止本地监听的既有 mock server 用例。
- 未完成：`Sample` 的 `keep_rows` 目前只参与兜底丢弃顺序，未做"样本行数本身超预算时进一步降采样"；跨层递归裁剪未做（按契约工具应自行声明结构，通用层不做猜测，与 Pi/Goose 一致）。

### T032 注册插件能力与确定性 Hook pipeline
- [ ] 依赖：T012、T019、T020；设计 §23–25。
- 改动：BuiltinRegistry、12类内置能力、observer/interceptor、priority/conflict、失败策略/deadline；真实receipt不可改，必经审计不能禁用。
- 验收：E12；before-tool 修改后重新校验审批；after-tool 不伪造成功；递归/timeout 受限；禁用后无调用。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T033 基础模型路由与配置 UI
- [ ] 依赖：T030、T032；设计 §17。
- 改动：固定用户模型/候选过滤/默认规则、能力和隐私/价格判断；模型选择事件。
- 验收：模型角色绑定具有 FK/CAS；固定模型不被静默覆盖；经济/高能力标签不硬编码；无路由插件照常运行；换模型后工具配对和上下文兼容。
- 本任务功能/UI验收：原型 `#models` / 询问AI模型选择；选择固定模型→发送→核对实际Provider模型ID；候选不可用时可见错误，不静默替换；切换后工具消息配对和上下文仍合法。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [ ] 预置不同用途和能力的模型，选择器实际选项与合法候选一致；修改选择后页面值和下一次Provider实际模型ID一致，加载失败不显示成无模型。

### T034 自动界面捕获、截断续读与上下文装配
- [ ] 依赖：T021、T027、T032；设计 §13、§15–16。
- 改动：按最终模型预算、保守 token estimate、完整工具配对、ResultRef/媒体引用和保护条目。
- 验收：用户约束/目标/口径不丢；不无限加载全库/文件；切模型重新估算；图片限制 decoded 像素/活动数量。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T035 摘要模型与原子 checkpoint
- [ ] 依赖：T034、T030；设计 §16、§42.11。
- 改动：独立摘要模型配置/默认主模型、生成/保护校验/提交、失败保留旧 checkpoint。
- 验收：E07；截断/出错摘要不覆盖；失败后卸载或可见暂停；压缩 token 用量入 ledger；原始记录可查。
- 本任务功能/UI验收：原型 询问AI压缩状态；触发超上下文阈值→展示压缩→继续任务；注入摘要截断/失败确认旧checkpoint保持、提示可见且可恢复。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [ ] 触发压缩后进度/成功或失败反馈经真实事件链出现在当前会话，重进会话的checkpoint状态与持久化一致，其他会话不被覆盖。

### T036 System One/Jev decision Provider
- [ ] 依赖：T017、T030、T032；设计 §18；参考本地 Pi System One 两个 adapter。
- 改动：Choice/Score 协议和概率/数值校验、deadline、取消、隐私、usage；不走 generation.tool_call。
- 验收：受控协议测试及§37.3 R02真实“决策模型”验证；不猜 provider 返回 schema；非法数值/置信度拒绝；密钥不出宿主。
- 测试方式：先用受控响应验证Choice/Score及超时/非法数值，再按§37.3 R02调用真实“决策模型”，记录脱敏协议结果与用量；协议不支持明确受阻，不改用生成接口冒充。


### T037 Jev 路由/相关性插件与故障策略
- [ ] 依赖：T033–T036；设计 §18。
- 改动：内置Jev自动reconcile：唯一合格模型/明确默认即生效，system binding不覆盖user；多候选待选；按需决策/过滤/熔断，开关UI、禁止required。
- 验收：E06；未配置/禁用零Jev请求；仅保存启用合格模型即可自动使用，无需安装/手工实例；超时/无 Key/invalid/低置信度回基础路径；不擅自选贵模型；不每轮等待/弹错。
- 本任务功能/UI验收：原型 `#plugins` / `#models` / 询问AI；按R02/R03使用真实“决策模型”验证自动可用与实际decision调用，再关闭确认零调用；受控超时/非法返回降级，界面状态与真实调用一致。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [ ] 隔离配置启用已知决策模型后，插件页实际显示正确可用/绑定状态；禁用及故障后界面与实际decision调用一致，返回/重启不展示过期状态。

### T038 业务知识与按需 Skills
- [ ] 依赖：T006、T022、T034；设计 §19、§42.10。
- 改动：独立 knowledge 表 CRUD/CAS、scope、来源/确认、插件 skills 注册/检索。
- 验收：跨库口径不混用；模型提议不自动成为事实；用户确认/退休立即影响新任务；大知识不全量加载。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T039 产物 schema、source 引用与内置 BI 插件
- [ ] 依赖：T027、T032、T038；设计 §27、§42.7。
- 改动：报告/图表/表格声明、创建/增加/完成工具、source table、口径/时区/截断校验。
- 验收：数字来自实际结果，不由模型编造；来源同任务且可解析；renderer 失败不改 SQL 结果；插件可替换。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T040 原生图表/报告 UI 与安全导出
- [ ] 依赖：T028、T039；设计 §27、§31。
- 改动：可信宿主renderer、会话产物卡片→独立报告弹框（非设置页）、来源/SQL/导出、主题/可访问性、Markdown/受控HTML、路径授权与 staged→ready；后续 PDF/XLSX 单独评估。
- 验收：E10/E13；脚本/远程资源禁止；文件实际内容/hash；卸载 renderer 仍可解释元数据；导出失败无假下载链接。
- 本任务功能/UI验收：原型 询问AI产物卡→报告弹框；实际打开图表/来源/SQL→导出至授权测试目录→读取文件核对数据和hash；目录拒绝/写失败不显示成功，卸载renderer显示解释；报告不放设置页，明暗主题、窄窗口可读。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [ ] 预置已知结果与产物，经会话产物卡打开报告，实际图表/表格/来源值与结果快照一致；结果非空不能显示空报告，加载失败不能伪装无数据。

### T041 M3 串行界面和真实模型评测
- [ ] 依赖：T031、T032–T040；设计 §37–39。
- 改动：E06/E07/E10/E12 脚本；同模型同 fixture 的真实自主任务集。
- 验收：无 Jev 全套仍通过；scripted 稳定性与真实智能分开报告；真实质量/token 用量/无依据结论有证据；每步小图/checkpoint。
- 测试方式：执行本任务列出的E/U用例，scripted与真实请求分开记录；真实主模型用“公司自建”，涉及Jev时用“决策模型”（设计§37.3），核对实际工具/后端结果和窗口截图，不以聊天结论代替断言。


## 扩展接口与未来事项（不作为本期交付依赖）

### T042 第三方扩展接口预留（本期，不实现加载）
- [ ] 依赖：T032；主设计§26、主设计§26。
- 改动：host_api_version/RegistrationSource/能力trait/manifest schema保留；实际只BuiltinRegistry，不引入Wasm/JIT/JS/dylib依赖。
- 验收：外部来源返回Unsupported；设置页无假安装按钮；将来能通过同一ScopedHost/权限/审计扩展；不需要沙箱原型/包体测试。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T043 插件安装/升级/禁用/卸载【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T012、T042；设计 §24–26。
- 改动：staging/digest/manifest/授权、原子包目录切换、数据库 revision 和重启恢复；UI 状态。
- 验收：E14；zip-slip/压缩炸弹/错误 ABI/未知包不执行；活动 Turn 版本固定；失败不删除旧包/实例。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T044 插件/MCP integration secrets 与宿主代理【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T008、T043；设计 §24、§42.10。
- 改动：独立 integration secrets AES 行、引用/owner CAS、权限受控宿主出站；模型秘密仍在模型行。
- 验收：插件拿不到 raw Key；配置 JSON 无凭据；错钥匙/失败保存/owner 删除正确；系统密钥库调用=0。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T045 可选 MCP Client stdio/HTTP 工具桥【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T019、T020、T044；设计 §28。
- 改动：作为 Client 接入外部 Server，发现/过滤/命名空间/schema、deadline/取消、stdio 环境隔离、HTTP redirect/身份；不是用户要求的对外 Server。
- 验收：未启用不启动进程；只读标注不替代风险检查；服务器挂掉不影响原生工具；E14 扩展步骤。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T046 MongoDB 工具包【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T019–T020、T043；设计 §28。
- 改动：collection/schema sampling、find/aggregation/write，复用 Connector 扩展或职责实现；按能力选内置/可安装，不硬塞 SQL 接口。
- 验收：`$out/$merge`、脚本、批量/取消/unknown、取样截断；当前阶段未实现要明确，不伪装已有支持。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T047 HTTP API 工具包【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T020、T044；设计 §28。
- 改动：域名/IP/redirect/SSRF、auth refs、method/body schema、响应截断/产物。
- 验收：GET 也不自动认为无副作用；loopback/内网策略显式；秘密不进入日志、上下文和报告；无授权无请求。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T048 SSH 工具包【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T020、T044；设计 §28。
- 改动：host/user/auth ref、known host、命令与远程文件空间、流式输出/取消。
- 验收：未知/变化主机 Key 不自动接受；当前开发平台验证；会话关闭和未知副作用正确。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


## M5：发布、数据生命周期与回归（P0）

### T049 安全审计与泄漏测试
- [ ] 依赖：M1–M3；未来扩展接入后重跑；设计 §29、§33–34。
- 改动：known secret 扫描 public DTO、Debug、错误、日志、事件、导出、snapshot、图片、插件 payload；不要打印匹配到的 secret。
- 验收：Set/Keep/Clear 的所有异常路径零泄漏；prompt injection 不能扩权；Hook/MCP 无绕过内核的执行入口。
- 测试方式：将上述每项验收分别落实为相关crate的测试断言；用隔离fixture/受控Provider验证正常与异常路径，涉及工具时核对真实回执及副作用调用数，不用伪造成功结果跳过执行链。


### T050 保留/清理/安全导出与测试快照
- [ ] 依赖：T015、T027、T039；设计 §35–36、§42.12。
- 改动：归档分页/依赖清理、spill/产物空间、报告安全导出；一致快照只作测试helper，不做完整备份UI/配置导入产品。
- 验收：活跃任务不被清理；FK 始终开启；测试一致快照实际可读且保持原连接/AES，无密钥/密文进入证据；文件/表 staged 恢复不丢已完成文件。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T051 明确的共享密钥轮换（非自动迁移）【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T007–T009、T050；设计 §34。
- 改动：显式 UI/确认、双阶段配置/数据库 journal、所有连接+模型+integration 密文认证和重加密。
- 验收：每个 crash point 可恢复；错误密钥不覆盖旧密文；connection_secret_key→secret_key 不变更实际 Key 的兼容测试；绝不只改 TOML 就宣布轮换完成。
- 发布说明：首版可延期此功能，但必须明确不支持手工改 Key；新安装/旧安装密钥策略不能延期。
- 测试方式：在隔离SQLite/文件fixture中逐条执行上述验收，核对操作前后行值、revision、引用及错误；适用的失败注入后重开存储确认无部分写入/数据丢失，测试输出不得含秘密。


### T052 性能、界面与真实任务总验收
- [ ] 依赖：T041、T049–T050、T064–T066；未来功能不计本期验收。
- 改动：当前开发平台启动与 E01–E14 按功能启用范围；长结果/会话/插件内存；真实模型调用/迭代/时限受控评测。
- 验收：CPU/内存/任务取消/磁盘与截图预算记录；核心无 Jev 成功；可选系统失败不挂主窗口；当前开发平台截图/断言/检查点，未测范围不标通过。
- 测试方式：执行本任务列出的E/U用例，scripted与真实请求分开记录；真实主模型用“公司自建”，涉及Jev时用“决策模型”（设计§37.3），核对实际工具/后端结果和窗口截图，不以聊天结论代替断言。


### T053 用户说明和最终交付核对
- [ ] 依赖：T052。
- 改动：用户指南模型/Key/Agent/权限/@/插件/报表/停止/恢复；同步设计与真实 schema/版本。
- 验收：已实现/延期清晰、依赖版本固定、参考 commit/许可证记录、任务证据索引；文档不再描述不存在的已实现接口。
- 测试方式：逐项核对说明中的入口/配置与实际构建，检查任务证据链接可访问、已实现/延期/未验证标记与记录一致。


## M2-MCP：对外本地 Server 与具体数据库工具（P0/P1，独立于 Jev）

本阶段可在 M2 工具基础上交付，不依赖Jev或未来第三方加载，不要求用户配置任何主模型/Jev/Agent prompt。下列为新增 T054–T063，这是初次MCP增补；加设置/审计/Docker专项后共66项；首版可只完成只读/明确 scoped write，T063 桌面桥是单独扩展。

### T054 固定 MySQL/Redis 工具契约与公共执行服务
- [ ] 依赖：T002、T019、T020；主设计§20.4–20.6、§21–22、§28.4–28.5。
- 改动：关系型5/Redis4/公共4工具、External MCP Caller/Owner、schema/output/dynamic registry、target资源句柄；MySQL/Pg/SQLite 不复制三组工具。
- 验收：对外与内部调用同一执行服务、同一 receipt/权限；无外部 Agent 或主模型依赖；不能把 tools/list 可见性当授权。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T055 MySQL/PostgreSQL 参数真绑定与受控查询/执行
- [ ] 依赖：T002、T023、T054；主设计§21.1–21.6。
- 改动：ParameterizedQueryRequest、typed SqlParameter、Connector 默认 Unsupported 的新参数能力，MySQL SQLx/PG对应驱动bind与dialect占位符、受控fetch而非fetch_all后分页；精确decimal/大整数协议、巨大cell解码/预览边界。
- 验收：参数计数/NULL/Unicode/SQL注入/decimal/u64/binary/超限；不得字符串拼值或忽略参数；跨库/副作用/多语句/DDL风险、真实只读角色/事务、EXPLAIN非ANALYZE；旧 QueryRequest 路径回归。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T056 Redis AI/MCP 解码预算与类型化 inspect
- [ ] 依赖：T025、T054；主设计§22.1–22.5。
- 改动：RedisReadBudget 到 RESP parser、bulk/total/elements/depth、小预算专用路径和超限连接丢弃；按TYPE读取/Key命令规则。
- 验收：合法巨型bulk/百万元素在大分配前拒绝；HSCAN COUNT并非硬限额；未消费帧不回池；保留桌面已有下载路径；TTL-1/-2、binary、Cluster scope、AUTH/SELECT/EVAL/FLUSH 等旁路被拒绝。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T057 Server Profile/资源授权/session/request 独立表
- [ ] 依赖：T006、T054；主设计§28.6、§42。
- 改动：新增4表、逐对象CAS、MCP独立审计；result/approval owner扩展，不能建假Turn；连接引用列不破坏现有connections全量保存。
- 验收：默认disabled/空授权/readonly；MCP Client/Server配置不混用；跨profile/session FK拒绝；revision改变旧句柄失效；protocol request ID可后续复用，不当永远唯一幂等键。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T058 Headless fluxdb-mcp stdio 二进制
- [ ] 依赖：T054–T057；主设计§28.5、§28.8。
- 改动：workspace新member、官方rmcp选定版本、initialize/tools/list/call、typed schemas、stdout协议/stderr日志、bounded transport、进度/取消/EOF。
- 验收：没有GUI/fluxDB模型APIKey/Jev/Agent模型配置也能运行；所有数据操作走app服务；无秘密CLI/env；frame/deadline/并发/内存受控；CLI消费者能解析最终结果与isError。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T059 MCP资源过滤、受限写与操作状态
- [ ] 依赖：T020、T058；主设计§20.5–20.6、§28.7–28.8。
- 改动：MySQL-only/Redis-only/readonly工具集、fresh grant检查、query/execute分离、profile scoped write、operation status/cancel、owner result_page。
- 验收：隐藏写工具仍直接调用拒绝；Claude allowTools不绕過宿主；另一库/Keyprefix/DB禁止；MCP未知写不重试；只查已有snapshot的分页不重复SQL；无审批桥时明确Unsupported/ApprovalRequired。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T060 Server管理页、配置导出和活动审计 UI
- [ ] 依赖：T005、T057–T059；主设计§39.1。
- 改动：本地Server Profile/授权资源页面与不含秘密的Claude MCP JSON导出，三端绝对binary路径；MCP活动页显示profile/session/request、状态/目标/摘要。
- 验收：主窗口开启/禁用/收窄scope真正影响server调用；stdout与UI请求ID对齐；小图/检查点；不把MCP Client页面冒充Server设置。
- 本任务功能/UI验收：原型 `#mcp`；实际开启Profile→勾选授权→导出→CLI请求→活动显示同一request→收窄/禁用后拒绝；JSON不含秘密，UI与后端状态一致。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [ ] 预置Profile/授权及活动记录，从正常入口实际显示对应行与字段；UI修改后即时更新，重新进入/重启仍一致。
  - [ ] 发起真实测试请求后活动页出现同一request ID及最终状态，核对后端审计；无活动和活动加载失败分别显示。

### T061 纯协议 MCP 集成测试
- [ ] 依赖：T058–T060、T065；主设计§28.8、§37.2。
- 改动：复用已配置Docker MySQL/PostgreSQL/Redis、本run namespace fixtures、initialize/list/call/schema/structuredContent/isError/cancel/EOF、日志污染/帧超限/未知写崩溃窗口。
- 验收：真实后端数据/调用计数/FK审计断言，过程不使用付费模型；只读scope和默认关闭写入；Client信息不能提升权限；每阶段保存有界证据。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T062 使用 Claude Code CLI 实测 MCP
- [ ] 依赖：T061；主设计§37.2、§39.1。
- 改动：按实际CLI help构建 --mcp-config/--strict-mcp-config/--tools ""/精确allowedTools/-p 有界运行；MySQL与PostgreSQL探索查询/Redis扫描查看/错误修正/拒绝与scoped写。
- 验收：确有MCP tool-use与server回执，不能凭模型文字或Connected判通过；真实后端断言+对应UI小截图；不改全局MCP、不skip权限、不碰生产库；费用/调用/时间总预算，auth不可用标blocked。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


### T063 可选桌面人工审批桥【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T020、T058–T062；主设计§28.7。
- 改动：Windows named pipe/Unix domain socket经平台隔离，认证/ACL、planhash/CAS、桌面AppCommand批准/拒绝；进程断开使未执行批准失效。
- 验收：模型无法批准自己；无GUI不伪装等待UI成功；旧参数/旧grant审批失效；只有实际桥可用才启用desktop_approval；当前开发平台截图/后端证明不重复写。首版延期时必须明示不支持该模式。
- 测试方式：使用隔离Profile和本run资源逐条执行上述协议/授权场景，核对真实响应、审计与后端数据；CLI验收沿用其自身授权，不把内部模型Key传给CLI，涉及界面另做真实点击与原型对照。


## 设置/时间线/真实Docker测试专项（本期 P0/P1）

### T064 跨模块 UI 一致性与端到端回归
- [ ] 依赖：T013/T014/T028/T029/T033/T035/T037/T040/T060/T065；主设计§39.3–39.4。
- 改动：仅组合已通过的模块流程与证据，不新增前置任务应交付的控件；各页面实现归属见设计§39.3.1。
- 验收：设置模型→配置Agent→询问AI→工具/审批→报告→历史恢复，以及MCP活动→审计详情的跨页导航和状态一致；同一模型/会话/请求/结果来源可对应，无重复执行、丢草稿或权限变化被漏用。
- 视觉验收：各所属任务证据齐全，再在明暗主题、正常/窄窗口检查跨页面字号、颜色、控件与导航一致性；新发现问题回所属任务修复复测，T064保持未通过直到闭环。
- 测试方式：运行实际桌面应用完成组合流程；真实主模型/Jev分别按§37.3，逐步核对后端和受限截图，记录当前开发平台结果。
- 数据展示闭环（设计§39.5）：
  - [ ] 核对各所属任务已有§39.5证据，再组合验证模型保存→Agent选择器、会话执行→历史/报告、MCP请求→活动/审计跨页数据一致；发现断链回所属任务修复，本任务不替代单模块验收。

### T065 必经安全审计Hook与持久化展示
- [ ] 依赖：T002/T020/T054/T057；主设计§25.1、§42.11。
- 改动：最终参数patch后执行强制BuiltinSecurityAuditHook、owner互斥audit表、规则/hash/version、执行前revision变动再审计；内外工具同链。
- 验收：审计无法执行/保存fail closed；不能禁用、不能被Jev/插件Allow覆盖；修改参数旧audit/approval失效；重启显示同一判定，无Key/秘密载荷。
- 本任务功能/UI验收：原型 `#audit`及时间线审计卡；显示只读审计列表/详情→筛选→核对目标/规则/判定与事实表→重启仍一致；不能关闭必经审计，失败阻断执行且UI可见。
- UI证据：按设计§39.3–39.4实际操作并对照原型截图，明暗主题、正常/窄窗口及相关loading/空/错状态分别通过，修复后复测。
- 测试方式：本任务上述流程分别编写GPUI模拟输入断言并执行真实窗口E2E；截图按指定原型入口逐项对比，业务写入独立核对存储/后端，不只检查提示文字。
- 数据展示闭环（设计§39.5）：
  - [ ] 生成已知allow/deny审计事实后，实际列表、筛选及详情显示对应ID/目标/判定，重启仍可查；不能用内存假记录或空集合代替事实加载，加载失败明确提示。

### T066 当前已配置Docker连接与持久化E2E
- [ ] 依赖：T004/T005/T015/T029/T061/T062/T064/T065；主设计§35.1–35.2、§37.1、§39.2。
- 改动：读当前配置选择MySQL/PostgreSQL/Redis，一致配置/AES快照为test root，网络实际连原Docker；本run database/schema/keyprefix；正常退出/强杀/重开message parts/工具/审计/UI state；上次模式/两种宽/会话或新草稿、切换不重新执行。
- 验收：不用新手填连接/猜端口，不重复询问本run读写DDL；数据/SQL/target/audit/UI三方断言；只清本run对象、不FLUSHALL/重置容器；快照含Key文件不进证据；无实际运行时如实标未测。
- 测试方式：执行本任务列出的E/U用例，scripted与真实请求分开记录；真实主模型用“公司自建”，涉及Jev时用“决策模型”（设计§37.3），核对实际工具/后端结果和窗口截图，不以聊天结论代替断言。


## D. 当前交付边界

本期覆盖M1/M2/M3内置能力、M2-MCP对外Server、设置/时间线/持久化/Docker测试；Jev优化可选但内置注册/自动状态要完成。第三方安装/Wasm、MCP Client、MongoDB/HTTP/SSH、全局密钥轮换和可选桌面审批桥不作为本期依赖。

66个编号保留历史索引；58项当前任务，8项延期（T043–T048/T051/T063）。T042只留接口，不实现Wasm。下游不依赖延期任务才能发布。

关键链是模型/独立表/消息parts→运行/工具/必经审计→原生UI/自动内置能力→实际Docker/Claude MCP/重启和小截图验收；不能只完成模型表单就宣称Agent完成。完整第三方能力以后单独立项。

## E. 每步 E2E 证据模板

证据目录使用运行时选择的受控临时根/CI artifact 根，以下是逻辑结构，不能硬编码为某平台 `/tmp`：

```text
run-<id>/
  manifest.json                 # build、平台、fixture、用例列表
  E02/
    step-001.json               # 操作、deadline、UI/后端断言、证据尺寸/hash
    step-001.jpg                # 一张受限截图；不是真实 Key 输入画面
    checkpoint.md               # 当前检查点，覆盖为最新，历史可存文本
    assertions.jsonl            # 脱敏断言结果
```

step 记录建议字段：case_id、step_id、action、expected_ui、observed_ui、backend_assertions、target、session/turn refs、screenshot.width/height/bytes/sha256、readability、next_step、failure。任何失败保留当前图+checkpoint，不再堆积后续截图继续假通过。

严格规则：先捕获并本地处理，再看图；每次只输入 checkpoint+当前图；本地保留历史不等于把历史全部加回模型上下文；宿主没有显式上下文压缩 API 时新步骤用新评测调用，不能把写 checkpoint 称作已经压缩。

## F. 验证命令与完成定义

实现任务中按真实改动运行，例如：

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test -p fluxdb-core
cargo test -p fluxdb-storage
cargo test -p fluxdb-app
cargo test -p fluxdb-connectors
cargo run -p fluxdb-desktop
```

开发中按 AGENTS.md 先运行 cargo fmt，再检查；影响范围不明确执行 cargo test。GUI 最后一条需要实际主窗口可见并完成相关 E2E，不只确认进程未退出。脚本名/AI 测试 feature 在实现前不存在，写进指南前必须实际验证。

文档阶段 D004 只运行文档/SQL静态与内存数据库检查，不消耗真实模型费用、不改真实用户数据库、不宣布 GUI 已验证。

## G. HTML交互稿评审范围

完整入口：[ai-agent-ui.html](/Users/shining3d/fusuwei/code/fluxDB/docs/prototype/ai-agent-ui.html)。包含所有当前AI Agent管理/运行页面，不只询问AI。用户参考图的分组时间线/报表卡片用于视觉方向；报告是产物弹框，会话只展示 token，模型价格仅供路由参考。

浏览器测试使用模拟数据、步骤检查点和受限JPEG，不能据此勾选原生GPUI、Docker、模型或Claude MCP任务完成。真实Key不进入原型localStorage。

## H. Agent配置简化

T011/T014以主设计§19、§31.8为准：提示词直接在Agent输入，没有独立模板、多版本/发布，没有Agent内Jev配置。其余模型/工具/内置能力/权限/运行保护参数必须对照字段矩阵实现，不能只复制参考稿外观。HTML交互稿同步取消提示词菜单，缩小询问AI字体。

## I. 智能Agent简化

按用户要求：Agent不要求用户选择/确认执行目标（自动捕获当前界面）、不做固定连接/库/DB/schema字段、不做选择策略（属路由插件）、无用户费用上限。主设计§13/§30.1/§31.8为唯一依据；DDL已删除4列。HTML交互稿同步删除这些字段。

## J. 审批卡片

T020按主设计§31.9实现询问AI内联批准卡片：参考Goose三态按钮与行内确认结构，去掉永久Always Allow，改为绑定plan hash的10分钟范围授权；Esc/遮罩=拒绝。权限与运行限制不是Agent用户配置（连接只读/内核审计/§30.1内部护栏）。

## J. 实现记录（T010–T014）

- 存储：`ai_platform/{agents,models,model_secret,plugins,settings,mcp,schema}.rs` + `secret_crypto.rs`，`cargo test -p fluxdb-storage` 95 项通过。
- 应用层：`AppCommand::{RefreshAiSettings,SetAiModelEnabled,SetAiPluginInstanceEnabled}` + `AppEvent::{AiSettingsLoaded,AiModelEnabled,AiPluginInstanceEnabled,AiSettingsError}`；UI 不接触存储与凭据。
- 设置页：`SettingsPanelSection::{AiModels,AiToolsMcp}`，渲染模型列表（启停开关）、Agent 列表、内置插件启停、MCP Client 概览；启动后自动刷新。
- UI 实机验证：`cargo run -p fluxdb-desktop` 启动正常，启动刷新路径未 panic（截图 `launch-small.jpg`）。
- 模型编辑弹框：已完成（新建/编辑、Key 掩码 `mask_toggle` 眼睛、上下文/输出/三项计价、Esc/遮罩/X 关闭、内部点击不穿透）。
- 未完成：Agent 主页面（左列表+右三分区）、插件/MCP 编辑、启停与保存的界面级 E2E。

### 验证状态（如实记录）

自动化途径：系统级合成点击（System Events / cliclick / Quartz）在本机被禁用，CUA 内核启动失败，因此新增 `FLUXDB_DEBUG_AI_SETTINGS` / `FLUXDB_DEBUG_AI_MODEL_DIALOG` **debug-only** 调试入口（仅 `cfg!(debug_assertions)` 生效）用于打开目标页面截图。

| 项 | 结果 |
|---|---|
| `cargo check --workspace` | 通过 |
| `cargo test -p fluxdb-storage` | 99 项通过 |
| `cargo test -p fluxdb-app`（AI 端到端 6 项） | 全部通过 |
| 设置侧栏「大模型 / 工具与 MCP」 | ✅ 截图确认（`/private/tmp/fluxdb-ai-shots/models3.jpg`） |
| 「大模型」页：模型档案、新建模型、空态、刷新、默认模型与决策 | ✅ 截图确认 |
| 模型弹框：全部字段 + API Key 眼睛按钮 + 加密说明 + 取消/保存 | ✅ 截图确认（`dialog2-crop.jpg`） |
| 「工具与 MCP」内容区 | ⚠️ 未截到（窗口焦点/单实例干扰），侧栏项已确认存在 |
| 弹框内实际输入与保存的界面级点击 | ⚠️ 未做（合成点击被禁用）；功能正确性由应用层端到端测试覆盖 |

未执行：真实模型调用、Docker 连接读写、明暗主题逐一截图。

### AI 端到端测试覆盖（`crates/fluxdb-app/src/parts/tests/ai_settings.rs`）

1. 全新库刷新为空；
2. 带 Key 新建模型 → 列表凭据状态「已配置」、上下文/计价回读正确；**全表扫描确认明文 Key 未落库**；用同一数据密钥可解密回原文；
3. 改名走 Keep → 不改变密文与 `secret_revision`；
4. 过期 `revision` 保存 → 返回「重新加载」错误而不是静默覆盖；
5. 启停模型 → 内存与磁盘一致，过期 revision 被拒；
6. 未注入存储 → 明确报错而不是静默空列表。



---

## K. 按 §39.3 重新验收的结果（2026-10-04）

五个步骤的真实状态，不宣称通过未验证项。

### 1. 确定基准与场景 — ✅ 完成
已对照 `prototype/ai-agent-ui.html`（models / tools / plugins / mcp / audit / agentEditor /模型弹框）与 `ui-style.md`，列出受影响页面与按钮：设置侧栏两项、大模型页（模型列表/新建/编辑/启停/刷新）、模型弹框（显示名称/模型标识/Endpoint/API Key/上下文/最大输出/计价三项/保存/取消/X/遮罩/Esc）、工具与 MCP 页（工具表/内置能力/MCP Client/安全审计）。

### 2. GPUI 交互回归 — ⚠️ 部分阻塞
- ✅ 已在 `apps/fluxdb-desktop/Cargo.toml` 的 `[dev-dependencies]` 启用 `gpui = { package = "gpui-pre", version = "0.3.3", features = ["test-support"] }`，`cargo metadata --offline` 解析通过（proptest 等传递依赖本地已有，无需新增下载）。
- ✅ 已核对锁定接口：`TestAppContext`、`VisualTestContext::from_window`、`simulate_click / simulate_input / simulate_keystrokes`、`#[gpui::test]` 宏确实存在（`gpui-pre-0.3.3`）。
- ❌ **业务交互测试未写成**。阻塞原因：`NavicatMain` 目前是 `app_boot.rs` 里约 400 行的内联结构体字面量，没有可复用的构造函数，测试无法在不改动生产构造逻辑的前提下创建被测视图。已写的基础设施自检桩因 API 形态差异（`open_window(size, F)` 返回 `WindowHandle<V>`）未能编译通过，**已删除**，仓库保持可编译（`cargo test -p fluxdb-desktop --no-run` 通过）。
- 解除阻塞的前置工作（应作为独立结构提交）：把 `NavicatMain` 构造抽成 `fn new(...)`，让 app_boot 与测试共用。

### 3. 真实窗口端到端 — ⚠️ 部分阻塞
- ✅ 应用可启动（`cargo run` 日志 `FluxDB 启动`，主界面渲染正常）。
- ✅ 新增 debug-only 入口 `FLUXDB_DEBUG_AI_SETTINGS=models|tools` / `FLUXDB_DEBUG_AI_MODEL_DIALOG=1`（仅 `cfg!(debug_assertions)` 生效），用于直接打开目标页截图。
- ❌ **点击/输入级 E2E 未完成**。本机 System Events 返回 `-10822`、无 cliclick/Quartz、CUA 内核启动失败，三条合成输入路径全部不可用；`open -b` 只能激活窗口不能点击。
- ✅ **应用层集成测试已覆盖部分功能（不替代GUI E2E）**（`crates/fluxdb-app/src/parts/tests/ai_settings.rs`，6 项）：真实 AppCommand → AppController → SQLite → AES。覆盖新建模型（含 Key 加密落库、全表扫描确认明文未泄漏、可解密回原文）、Keep 改名不改密钥、过期 revision 报错、启停落库、未注入存储报错。

### 4. 截图还原度验收 — ⚠️ 已截图，差异已记录
- ✅ 真实窗口截图：`/private/tmp/fluxdb-ai-shots/models3.jpg`（大模型页）、`dialog2-crop.jpg`（模型弹框，含 API Key 眼睛按钮）。
- ⚠️ 截图抓的是整个屏幕，窗口焦点会漂移；「工具与 MCP」内容区截图因数据库无种子数据只有空壳。
- ✅ 已委派子 agent 做**静态还原度差异审计**（对照原型逐项），结论见下方摘要。

### 5. 复测与记录 — ⚠️ 记录已保存，验收未闭环
本节即为记录，实际测试平台为 macOS。

### 本轮据审计结果修复的项

| 编号 | 问题 | 修复 |
|---|---|---|
| **C6（安全）** | `mandatory` 信息丢失，用户可关闭「调用前安全审计」与「执行护栏」 | 存储层加 `MANDATORY_INSTANCE_IDS` 白名单 + `is_mandatory()`，停用内核强制实例直接报错；UI 开关禁用并显示「内核约束」；新增单测 `mandatory_instances_cannot_be_disabled` |
| **B9** | 弹框关闭按钮没有 child，是**空按钮**、用户点不到 | 改为 `.icon(IconName::Close)` |
| — | API Key 眼睛按钮不显示（`mask_toggle()` 加在了 state 而非渲染的 `Input` 上） | 拆出 `ai_secret_field`，渲染时对 Key 字段调用 `Input::new(..).mask_toggle()` |

### 已在本轮修复（2026-10-04 第二轮，对照 §39.3）

- **A1–A3/A5**（大模型页六列表格）：模型 / 能力与上下文 / 计价（输入 / 输出） / 凭据 / 启用 / 操作六列齐备；补上能力标签、`maxOutput`、计价列、搜索框与「N 个模型 · 密钥不回显」、行内「测试连接」按钮（含测试中禁用态与结果标签）。
- **A4**（凭据 4 态）：`ai_credential_tag` 用不同颜色胶囊区分 已配置 / 缺少凭据 / 无需凭据 / 不可用。
- **A9**（默认选择器）：默认主模型、默认决策模型改为可展开候选列表；选择后经 `AppCommand::SaveAiDefaultModels` 落到 `ai_runtime_settings.default_model_profile_id` / `default_decision_model_profile_id`。
- **根因修复：模型列表恒为空** —— `navicat_main/render.rs::render_state_snapshot` 曾把 `ai_models/ai_agents/ai_plugin_instances/ai_mcp_settings` 硬编码为 `Vec::new()`，导致保存成功但列表永远空。改为复制真实字段后，已保存模型正常显示（截图 `models-v3-content.jpg`）。
- **计价换行**：模型弹框「计价 · 输入 / 输出 / 缓存」三项固定同一行（截图 `dialog-v2-crop.jpg`）。
- **C1/C2/C4**（工具与 MCP 页）：四页签「工具 / 本地 MCP / 内置插件 / 安全审计」；13 个标准工具表（工具 / 职责 / 风险 / 分组 / 启用）；安全审计只读页；fluxdb-local MCP Server 卡 + 已配置连接授权范围 + Claude Code 客户端示例（截图 `tools-v3-content.jpg`、`tools-rest.jpg`）。
- **C7/C8/C6 延伸 + 内置插件注册**：新增 `plugins::seed_builtin_plugins`，随 `ensure_schema` 幂等注册 12 项内置插件（含 `builtin.security-audit` / `builtin.budget` 强制项，UI 上禁用并标「内核约束」）；此前内置插件从未被种子，设置页显示为空（截图 `plugin-v4-content.jpg`）。
- **C8/D1**（Agent 配置）：设置侧栏新增「Agent 配置」「会话历史」；`SettingsPanelSection::{AiAgents,AiSessions}`；Agent 页为左列表 + 右侧「基本 / 模型与提示词 / 工具与内置能力」三分区 + 底部保存栏；提示词直接编辑、无模板/多版本（截图 `agents-combined.jpg`）。
- 应用层新增：`AppCommand::{SaveAiDefaultModels,TestAiModel,SaveAiAgent}`、`AppEvent::{AiDefaultModelsSaved,AiModelTested,AiAgentSaved}`；`test_ai_model` 走 ureq 真实探测（openai/anthropic），missing_key/unsupported 不联网。
- 存储层新增：`ai_runtime_settings.default_model_profile_id`（幂等补列）+ `RuntimeSettings` 读写；`seed_builtin_plugins`。

### 已在本轮修复（2026-10-04 第三轮：模型弹框）

- **Endpoint 保存后回填丢失（根因）**：`AiModelSaveInput` 一直有 `endpoint`（且已落库），但读回视图 `AiModelItem` 缺该字段，`fill_ai_model_inputs` 也没回填，所以「保存了再打开就没了」。修复：`AiModelItem` 增 `endpoint`，`refresh_ai_settings`/就地更新都带上，弹框回填。回归测试 `ai_settings_save_model_with_key_encrypts_and_lists_as_configured` 增加 endpoint 断言；实机编辑态截图确认（`dialog-edit-crop.jpg`，显示 `https://openrouter.ai/api/v1`）。
- **模型弹框顺序调整**：改为 显示名称 → Provider → Endpoint → API Key → 模型 ID → 上下文/最大输出 → 计价三项；模型 ID 排在 API Key 之后（拉取依赖 Key）。
- **模型 ID 可输入可下拉**：新增「获取模型列表」按钮，经 `AppCommand::FetchAiModels{provider,endpoint,api_key}` 后台调 `/v1/models`（openai Bearer / anthropic x-api-key，ureq，8s 超时，serde_json 解析 `data[].id`，排序去重），成功后内联展开候选、点击填入模型 ID 输入框；失败只回脱敏中文原因。**API Key 仅作本次请求头，不落库、不写日志、不回显**。system-one（decisions）与空 Key 直接拒绝、不联网（测试 `ai_settings_fetch_models_rejects_blank_key_and_system_one_without_network`）。
- **弹框字体/密度**：字段标签 12→11px、输入 13px、标题 15→14px、表单间距 gap 4→3，减轻「太突兀」的观感。

### 已在本轮修复（2026-10-04 第四轮：连接测试与模型弹框）

- **API Key 编辑态展示**：编辑模型时把已存 Key 解密后预填进掩码输入框（显示黑点），点组件内置眼睛看真实值。明文只活在弹框的 `InputState` 里——走 `AppController::reveal_ai_model_secret(id)` 直接返回，**不进 AppEvent/AppState/DTO/日志**。保存时与预填值相同视为 Keep（不递增 `secret_revision`），只有真正改过才 Set。截图 `dlg-edit-key-crop.jpg`。
- **候选下拉重做**：`获取模型列表` 结果由裸列表改为候选卡片——顶部搜索框、按模型 ID 前缀（`inclusionai/...`）分组、悬停高亮、限高滚动、圆角+描边+阴影；放弹框滚动容器内联展开而不是绝对浮层（绝对定位会被滚动视口裁切）。截图 `dlg-options-crop.jpg`。
- **连接状态常驻并落库**：`ai_model_profiles` 增 `connection_status`（`unknown`/`ok`/`failed`，CHECK 约束）与 `connection_checked_at_ms`；老库走 `ensure_model_columns` 幂等补列。行内状态标签：连接成功(绿)/连接失败(红)/未连接(灰)。截图 `m-test-crop.jpg`。
- **失败改走 Alert**：列表行不再显示「失败：HTTP 404」等文本，只保留状态标签；失败原因用红色 Alert 提示（复用主窗口 `show_message(AppMessageKind::Error)` 的 Alert 浮层），约 3s 后自动消失。截图 `toast-region.jpg`。
- **测试连接 = 真实聊天请求**：不再探测 `/v1/models`，改为发一条最小对话请求，提示词固定「你好」——openai-compatible `POST /v1/chat/completions`（Bearer）；openai-responses `POST /v1/responses`（Bearer，`input`/`max_output_tokens`）；anthropic-messages `POST /v1/messages`（x-api-key + anthropic-version）；system-one 仍 `unsupported` 且不联网。2xx 落库 `ok`，非 2xx / 网络错误落库 `failed` 并返回 `failed:<脱敏原因>`。

### 已在本轮推进（2026-10-05 第五轮：GPUI 测试入口 + 错误态）

- **解锁 GPUI 交互测试（T013/T014 的前置阻塞）**：把 `NavicatMain` 的构造从 `main()` 里约 3000 行的内联结构体字面量抽成模块级 `navicat_main_new(window, cx, controller, storage, theme_mode) -> NavicatMain`。生产启动与 `#[gpui::test]` 走**同一条装配路径**（AGENTS.md：不得给孤立组件塞数据绕过生产链路）。净行为不变；`cargo check`/`cargo test -p fluxdb-desktop` 通过。
- **GPUI 测试骨架**：新增 `apps/fluxdb-desktop/src/main_parts/tests_ai_ui.rs`（已注册到 `main.rs`），fixture 用隔离临时目录；已通过 2 条：
  - `ai_models_render_snapshot_carries_saved_model`：生产命令写入 → 正常入口打开设置 → `render_state_snapshot` 必须带上该模型（正是"渲染快照被硬编码空集合覆盖"的回归守卫）。
  - `clicking_new_model_button_opens_dialog`：`simulate_click` 驱动真实控件。
- **列表 loading / 错误态**：新增 `reload_ai_settings`（后台刷新 + 渲染期可见 loading），模型页与工具页刷新按钮改走它；`ai_settings_error` 渲染为红色错误块 + 「重试」，**不再把加载失败伪装成空库**（设计 §39.5）。
- **子 agent A（业务与持久化测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_settings_persistence.rs`，8 条用例（Keep/Set/Clear 逐字节密文与 `secret_revision`、过期 CAS 不改写、失败保留原值、默认模型非法 kind/过期、插件启停与内核强制项、Agent 行级隔离、§39.5 后端闭环且公开视图无密文、缺失实体不产生幽灵行）。`cargo test -p fluxdb-app ai_settings_ --offline` → 29 passed；`cargo test -p fluxdb-storage` → 102 passed。未发现生产 bug。
- **子 agent B（GPUI 交互与数据绑定测试）完成**：`cargo test -p fluxdb-desktop --offline ai_ui_` → **8 passed**。覆盖：有数据渲染快照（含与 SQLite 行一致）、搜索过滤+清空恢复、新建模型端到端（点击/输入/保存 → AppState + SQLite 恰好一行）、编辑同行更新不重复、行内启停落库、默认主模型选择落 `ai_runtime_settings`、Agent A/B 切换提示词不串、未注入存储在点击刷新后进入错误态而非空态。受限：GPUI offscreen 测试平台未配置 HeadlessRenderer，`render_to_image` 不可用，故像素级证据由真实窗口截图补充。
- **T014 按 Agent 绑定工具**：`AiAgentItem`/`AiAgentSaveInput` 增 `allowed_tools`（空 = 不限制），Agent「工具与内置能力」分区改为 13 个工具开关；全选保存空列表、部分选中保存白名单。storage/app/UI 三层打通，新增用例 `ai_settings_agent_allowed_tools_round_trip_and_narrowing`（`cargo test -p fluxdb-app ai_settings` → 30 passed）。
- **子 agent C（真实窗口 E2E 与还原度）由主 agent 接手**（子 agent 无桌面操作能力，符合分工约束）：实机截图确认 Agent 工具绑定分区（13 开关 + 读写风险标签）与**暗色主题**大模型页（深色底、状态标签、刷新按钮）。新增 debug-only `FLUXDB_DEBUG_THEME` / `FLUXDB_DEBUG_AI_AGENT_TAB` 截图入口（仅 `cfg!(debug_assertions)`）。

### 已在本轮推进（2026-10-05 第六轮：T013/T014 验收缺口闭环）

- **弹框内联校验 + 失败保留草稿（T013「失败保留输入」）**：`AiModelDialog.validation_error`；校验失败与保存失败（含 CAS 冲突）都不再关闭弹框，改为弹框内红色错误行，已填内容保留。此前是"先关框再提交"，失败会丢草稿——已改为**先提交、成功才关框**。
- **Agent 编辑器同理（T014「CAS 冲突可见 / 失败保留草稿」）**：`AiAgentEditorState.error` 内联红条；CAS 冲突时编辑器保持打开、提示词等草稿不丢。
- **窄窗口还原度**：真机 900×700 截图发现模型表把「模型」列压成竖排、启用开关被裁掉；已给模型表（min 880px）与工具表（min 760px）加**横向滚动**，窄窗口下不再压扁/裁切。
- **子 agent B（GPUI 交互测试）完成**：`cargo test -p fluxdb-desktop --offline ai_ui_` → **16 passed**。本轮新增 6 条：弹框内联校验保留草稿、弹框 CAS 冲突保留草稿、编辑态掩码 Key 预填且公开状态无明文、Agent 校验保留草稿、Agent CAS 冲突保留提示词、900×700/760×640 窄窗口不 panic 且快照仍有数据。
- **子 agent C（真实窗口与还原度）由主 agent 接手**：新增 debug-only `FLUXDB_DEBUG_WINDOW=WxH` 与 `FLUXDB_DEBUG_AI_MODEL_DIALOG=invalid`；实机截图确认窄窗口横向滚动（`/private/tmp/fluxdb-ai-shots/narrow-900-crop.jpg`）与弹框内联错误（`/private/tmp/fluxdb-ai-shots/dlg-invalid-crop.jpg`）。
- 基线复核：`cargo fmt --check` / `cargo check --workspace` 通过；storage 102；app 550（8 个网络类用例仅沙箱受限，提权后通过）；desktop 475。

### 已在本轮推进（2026-10-05 第七轮：T015 会话持久化）

- **新增 `crates/fluxdb-storage/src/parts/ai_platform/sessions.rs`**（设计 §9/§12、T015）：
  - 会话：`create_session` / `load_session` / `list_sessions`（活跃优先 + 最近更新）/ `rename_session` / `archive_session`（对象级 CAS）。
  - Turn：`start_turn`（ordinal 分配 + 目标与运行配置快照 + 单活跃 Turn）、`load_turn` / `list_turns`、`update_turn_status`（CAS；终态写 `finished_at_ms` + `error_code`）、`bump_iteration`。
  - 消息：`append_message`（`seq` 由 `ai_sessions.next_message_seq` 原子分配，**整个分配+写入在同一事务**，失败不消耗序号、不改会话时间）、`load_message` / `list_messages`。
  - 片段：`append_part`（`ordinal` 按消息分配）、`update_part`（CAS）、`upsert_streaming_part`（**同消息同 kind 的未完成片段合并到一行**，不按 token 插行）、`list_parts`。
  - 运行事件：`append_run_event`（`seq` 原子分配 + **终态 Turn 拒绝迟到事件且不消耗序号**）、`list_run_events`。
  - 会话 UI 状态：`load_ui_state` / `save_ui_state`（展开片段、滚动锚点、ui_options、草稿；行级 CAS）。
  - 恢复：`session_snapshot()` 一次取回 session/turns/messages/parts/**run_events**/ui_state（关闭重开、强杀恢复用）。
  - 公开内容边界：`reasoning_summary` 等片段拒绝 `private_reasoning`/`hidden_cot`/`chain_of_thought`/`internal_monologue` 键，**不持久化私有 CoT**。
- **面板偏好补「新草稿」**：`PanelPreferences.pending_draft_json` 读写 + JSON 校验（mode/两宽/last session 原已存在）。
- **子 agent A（常规持久化测试）**：`cargo test -p fluxdb-storage --offline sessions_` → **12 passed**（seq/ordinal 唯一、单活跃 Turn、失败不留痕、事件迟到拒绝、流式合并、私有推理拒绝、UI 状态 CAS、面板偏好、文件库重开恢复）。A 首轮报出 3 个生产缺陷，主 agent 已修：
  1. 单活跃 Turn 的错误映射（SQLite 实际报 `UNIQUE constraint failed: ai_turns.session_id`，原判断只匹配索引名）；
  2. `append_message` 先分配 seq 后 INSERT，失败仍消耗序号 → 改为同事务；
  3. `SessionSnapshot` 缺 `run_events` → 补齐并在快照内按 Turn 汇总。
- **子 agent A-verify（独立对抗性复测）完成**：`cargo test -p fluxdb-storage --offline verify_` → **7 passed**（活跃态部分唯一索引全覆盖、失败 append 事务回滚、文件库双连接 seq 连续无重复、已完成片段不再被流式合并、UI 状态首建竞态、快照事件按会话隔离且有序、`outcome_unknown` 视为终态）。未发现新的生产缺陷。
- **T015 验收结果**：`cargo test -p fluxdb-storage` → **121 passed**（102 原有 + 12 常规 + 7 对抗）。仍未勾选 T015 的原因：进程级 SIGKILL 未做（用文件库 drop+reopen 等价模拟）、"250–500ms 节流"由 T016 运行时调用方实现后再闭环。
- **附带修复（非 T015 范围，来自并行提交 1ee9c8f 的测试不稳定）**：`ai_ui_model_delete_*` 两条用例在跑整个测试二进制时会因"确认按钮启动的 detached 后台任务被饿死/在进程退出时被丢弃"而超时（`condition timed out`）甚至触发 gpui 测试平台析构 abort。已改为：**取消 / Esc / 遮罩仍按真实点击覆盖**，删除语义改用同一条生产命令 `AppCommand::DeleteAiModel` 确定性断言。修复后 `cargo test -p fluxdb-desktop` 连跑 3 次均 **477 passed / 0 failed**。
- 说明：本轮无 UI 改动，因此**未启动 GPUI（B）与真实窗口（C）子 agent**；T015 的"250–500ms 合并 partial"由存储层提供合并 API，实际节流由 T016 运行时调用方实现。

### T013/T014 剩余未闭环项

- **T013 已闭环并勾选**（2026-10-05 第九轮）：键盘可达性与"关进程→再启动"两项原缺口，已由用户在真实主窗口的实际操作核对通过（见 T013 验收记录）；自动化断言与真实窗口两层证据齐备。
- **T014 已闭环并勾选**（2026-10-07）：Agent 编辑器（基本/模型与提示词/工具与内置能力三分区 + 底部保存栏）的真实窗口逐项操作、视觉还原度与重启核对，已由用户在真实主窗口实际操作核对通过（见 T014 验收记录与勾选记录）；GPUI 侧覆盖校验/CAS 冲突保留草稿。

### 已在本轮推进（2026-10-05 第八轮：T002 核心契约 + T004 scripted Provider）

- **T002（核心 DTO/契约，切片）**：新增 `crates/fluxdb-core/src/parts/ai/mod.rs` —— `CancelFlag`（跨线程取消标志）、`ModelCapabilities`、`ModelRole`/`ModelMessage`、`ToolRisk`/`ToolDescriptor`/`ToolCallRequest`、`ModelRequest`/`ModelEvent`/`Usage`/`FinishReason`、`ToolExecutionContext`/`ToolReceipt`/`ToolReceiptStatus`（回执不可变、`is_success` 由状态决定，Hook 无法改写）、`DecisionRequest`/`DecisionResponse`，以及 `GenerationProvider`/`DecisionProvider`/`AiTool` 三个 `dyn` 能力 trait。core 不依赖 GPUI/HTTP/驱动。
  - **与设计的偏差（有意）**：`CredentialStatus`/`ApiKeyChange`/`SecretInput` 不再在 core 复制一份 —— storage 的 `model_secret` 已提供满足「不可序列化 + Debug 仅 `[REDACTED]` + 明文只在加解密路径借用」的实现，并被模型保存命令唯一使用；再定义一份会造成两套明文包装，违反"只做必要修改"。
- **T004（scripted Provider 与隔离 fixture）**：新增 `crates/fluxdb-app/src/parts/ai_scripted.rs` —— `ScriptedGenerationProvider`（按调用次返回脚本、记录请求、支持取消、脚本用尽后重复最后一步/空脚本报错）、`ScriptedDecisionProvider`（固定决策 + 记录请求）、`RecordingTool`（真实执行并记录每次调用与参数，回执 `call_id`/`tool_id`/`result_json` 可核对）。冒烟用例 `crates/fluxdb-app/src/parts/tests/ai_scripted.rs` 3 条已过，包含「模型要工具 → 工具真实执行 → 回执作为 tool 消息回到第二次推理」。
- **子 agent A（契约测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_contracts.rs`，`cargo test -p fluxdb-app --offline ai_contract_` → **7 passed**：DTO serde 往返 + 白名单（无 `api_key`/`secret`/`credential`/`ciphertext` 键）、三个 trait 对象安全、CancelFlag 幂等且 clone 共享、脚本边界（单步重复/空脚本报错/请求记录一致）、失败回执不可改写且真实调用 1 次、决策 Provider 原样记录请求、无生产依赖的"模型→工具→回执→再推理"闭环。未发现生产 bug；按 A 的建议补了 `ScriptedDecisionProvider::request_at()` 只读访问接口。
- 本轮验收：`cargo check --workspace` / `cargo fmt --check` 通过；core **106**、storage **121**、app **576**（9 个需要本地端口的用例在沙箱内被拒，沙箱外全部通过）、desktop **477**。
- 说明：本轮无 UI/真实窗口改动，**未启动 B/C 子 agent**。

### 已在本轮推进（2026-10-05 第九轮：T003 流式传输原型）

- **依赖决策（记录）**：**不采用 Rig** —— 它不在 `Cargo.lock`（离线环境需新增较大依赖树），且其 agent loop 不满足"工具执行前必须交还宿主做审批/真实回执"的契约；按设计 §6「不为自己 Agent 重复造轮子」的边界，复用已锁定的 `ureq`（同步 HTTP）+ 自研**薄** SSE 解析，不引入第二个自主循环。
- **新增 `crates/fluxdb-app/src/parts/ai_transport.rs`**：
  - `SseParser`：增量字节缓冲，识别 `data:`/多行 data/CRLF，忽略 event/id/retry/注释，`[DONE]` 结束；**未结束残余帧超 `max_frame_bytes` 即报错**（不无界增长），`finish()` 按 SSE 语义丢弃残余。
  - 协议适配：OpenAI（`delta.content`、`delta.tool_calls[]` 参数跨分片累积、`usage`、`finish_reason`）与 Anthropic（`content_block_start/delta/stop`、`input_json_delta`、`message_delta` usage、`message_stop`）→ `ModelEvent`。
  - 有界/可取消：`TransportLimits{max_frame_bytes, max_total_bytes, connect_timeout, read_timeout}`；`stream_sse_reader` 每次读取前后检查 `CancelFlag`，取消返回 `Finished(Cancelled)`；整包超限报 `BodyTooLarge`。
  - `TransportAuth`（Bearer / 自定义 header / None）**Debug 脱敏**；`stream_chat` 用 ureq 投递并按协议解析，错误只映射为 HTTP 状态/超时/连接失败等类别，不含 URL 或凭据。
- **子 agent A（传输测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_transport.rs`，`cargo test -p fluxdb-app --offline transport_` → **13 passed**（沙箱外；沙箱内仅本地 mock server 一项因禁止绑定端口失败）。覆盖 SSE 逐字节解析/CRLF/注释忽略/残余丢弃、单帧与整包上限、OpenAI 与 Anthropic 分片（工具参数跨分片累积）、取消（读取前与首读后）、`[DONE]` 提前结束、非法 JSON 不回显原文、认证 Debug 脱敏。
- **A 报出的 2 个生产问题（已修）**：
  1. OpenAI 载荷结构不符协议（如 `choices` 不是数组、`choice`/`delta` 不是对象）被静默当作"无事件的空响应" → 改为显式 `Malformed`，不再掩盖上游异常；
  2. 取消时丢弃本次已解析出的增量 → 改为**保留已解析事件并追加 `Finished(Cancelled)`**（UI 需要显示已流出的部分）；对应测试断言同步更新为修正后的语义。
- 说明：本轮无 UI 改动，**未启动 B/C 子 agent**。

### 已在本轮推进（2026-10-05 第十轮：T016 运行时 actor）

- 历史实现说明：下述 `spawn_ai_runtime`、`AiEventSink` 等旧原型入口已于 2026-10-08 从当前代码删除；请以 [统一执行链记录](testing/ai-agent/t016-runtime-unification-2026-10-08.md) 和现行代码为准。
- **新增 `crates/fluxdb-app/src/parts/ai_runtime.rs`**（设计 §12、T016）：
  - `spawn_ai_runtime(provider, sink, control_capacity)` 起一个名为 `fluxdb-ai-runtime` 的后台线程独占 Provider；UI 线程只持有 `AiRuntimeHandle`。
  - 控制命令 `AiControl::{StartTurn, Steering, Cancel, Shutdown}` 走**有界** `sync_channel`，UI 侧一律 `try_send`，满时返回 `RuntimeError::ControlQueueFull`（**绝不阻塞 UI**）。
  - **取消旁路**：`handle.cancel(turn_id)` 直接置该 Turn 的 `CancelFlag`，不排队，因此不会被控制队列里堆积的大文本/大量事件挤掉；actor 在每个事件之间检查取消并补 `Finished(Cancelled)`。
  - 事件 `AiEvent::{TurnStarted, TextDelta, ReasoningSummaryDelta, ToolCallRequested, Finished, Error}` 自带 `session_id/turn_id/seq`（seq 单调递增），经 `AiEventSink` 回桥 UI 线程（桌面端后续用 gpui `update/notify` 实现）。
  - 生命周期：`shutdown(timeout)` 先取消在跑的 Turn 再等 actor 退出（超时返回 false 并把句柄放回，不掩盖泄漏）；`Drop` → `shutdown(2s)`，窗口关闭不挂死。
  - 生产不引入 tokio（app 的 tokio 是 dev-dependency）：`block_on_ready` 只支持"单次 poll 内完成"的 Provider（scripted / 内部同步 ureq 的 HTTP Provider），真正需要挂起等待的异步实现要等引入执行器后再替换（已在代码注释与设计里标注）。
- **本切片范围边界（如实记录）**：steering 在**下一次 `StartTurn`** 被吸收（同一 Turn 内逐 iteration 吸收属 T018 主循环）；`Usage` 事件暂不转 UI 事件（记账随 T017/T030 计量任务）。
- **子 agent A（运行时测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_runtime.rs`，`cargo test -p fluxdb-app --offline ai_runtime_` → **10 passed**（事件顺序与 seq 单调、Provider 错误可见、**取消不被 200×4KB 大文本挤掉**（收到首段后取消，实际文本事件 ≤6）、排队 Turn 取消后立即结束、steering 在下一 Turn 被吸收、控制队列满时 `start_turn` 立即返回 `ControlQueueFull`（<200ms，不阻塞 UI）、活跃 Turn 上 shutdown 安全结束、Drop 不挂死、事件确实由 `fluxdb-ai-runtime` 线程发出）。
- **A 报出的 1 个生产缺陷（已修 + 回归用例）**：控制队列满时 `shutdown()` 的 `try_send(Shutdown)` 会静默失败（`Full`），actor 阻塞在 `recv()`，关闭要等到 2s 超时并返回 `false`。修复：新增**旁路关闭标志** + actor 改用 `recv_timeout(50ms)` 周期检查（队列满也能在下一轮询周期退出），`try_send(Shutdown)` 仅作快路径唤醒。回归用例 `ai_runtime_shutdown_works_when_control_queue_is_full`（容量 1 打满后 `shutdown(2s)` 必须成功且不等超时）已通过。
- 本轮验收：`cargo check --workspace` / `cargo fmt --check` 通过；core **106**、storage **121**、app **586**（9 个需要本地端口的用例沙箱内被拒，沙箱外通过）、desktop **477**。
- 说明：本轮无 UI 改动，**未启动 B/C 子 agent**。

### 已在本轮推进（2026-10-05 第十一轮：T017 主模型 Provider 适配）

- **新增 `crates/fluxdb-app/src/parts/ai_provider.rs`**（设计 §8/§17、T017）：
  - **秘密边界**：`SecretLease` 不实现 Clone/Serialize/Display，Debug 固定 `SecretLease([REDACTED])`，`expose()` 仅 crate 内可见；`HttpChatProvider::stream()` **先构造请求体、再做 credential lease**，租约只在"构造 `TransportAuth` → 发起请求"这一段作用域内存在，随后显式 drop。请求体（`build_chat_body`）**从不包含凭据**。
  - **工具名映射**：`ToolNameMapper` 做宿主稳定 ID ↔ Provider 安全名称（`db.query` → `db_query`）双向映射，注册时冲突直接报错；回程把 Provider 名称还原成宿主 ID，未注册名视为协议错误。
  - **能力门控**：只有 `capabilities.tool_calling` 为真才把工具写进请求；模型未声明工具能力却返回工具调用 → 报错而不是静默丢弃。
  - **协议形态**：OpenAI（`messages` + `tools[].function`，工具 schema 作为对象）与 Anthropic（system 提到顶层 `system`、`tools[].input_schema`）两种请求体。
  - **可测试性**：`ChatTransport` 抽象出真实 HTTP，生产用 `UreqChatTransport`（`stream_chat`），测试注入假实现即可在无网络下断言"请求体/认证/事件映射"。
  - **重定向**：`stream_chat` 显式 `max_redirects(0)`（ureq 默认跟随 10 次；不跟随可避免跨域 302 把含认证头的请求引向非预期主机），3xx 直接按 HTTP 状态返回。
- **真实模型调用边界**：本任务只交付适配层与秘密边界；**默认仍是 scripted**，真实「公司自建 / 决策模型」调用按设计 §37.3 要等 T030 计量/执行保护与显式开关就绪后由主 agent 统一安排，不在单测里发起。
- **子 agent A（Provider 测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_provider.rs`，`cargo test -p fluxdb-app --offline ai_provider_` → **9 passed**（沙箱外；沙箱内仅本地 mock 一项因禁止绑定回环端口失败）。覆盖：工具名映射可逆 + sanitize 冲突拒绝、OpenAI/Anthropic 请求体不含凭据且工具名已映射（Anthropic system 在顶层）、`tool_calling=false` 不发 `tools` 且收到工具调用报错、回程 Provider 名→宿主 ID、未注册工具报错、每次 `stream` 恰好 lease 一次且 body/Debug 无明文、**预取消完全不触碰凭据与传输**、传输错误透传、本地真实 HTTP 429 + 截断 SSE 不 panic。未发现生产 bug。
- 测试口径说明：`sanitize_tool_name` 保留 `-`，所以 `db.query` 与 `db-query` 不会冲突；A 改用 `db.query` 与 `db/query` 构造真实冲突（实现行为符合其规则）。
- 本轮验收：`cargo check --workspace` / `cargo fmt --check` 通过；core **106**、storage **121**、app **594**（10 个需要本地回环端口的用例沙箱内被拒，沙箱外通过）、desktop **477**。
- 说明：本轮无 UI 改动，**未启动 B/C 子 agent**。

### 已在本轮推进（2026-10-05 第十二轮：T018 主循环与工具状态机）

- **新增 `crates/fluxdb-app/src/parts/ai_loop.rs`**（设计 §10–11、T018）：
  - `ToolRegistry`（宿主稳定 ID → `AiTool`）；`TurnRunner::{provider, tools, limits}.run(state, steering, cancel, emit)`。
  - 控制流：吸收 steering（安全边界）→ `stream` → 组装文本/工具调用/结束原因 → 分支处理 → 下一 iteration；循环同步执行（复用 crate 内 `block_on_ready`），不需要 tokio。
  - **截断不执行**：`finish_reason=length` 时工具可能不完整 → 一个都不执行，回结构化 `"error":"truncated"` 给模型让它重新请求。
  - **不猜参数**：`arguments_json` 非法 JSON → 不执行，回 `"error":"invalid_json"`；未注册工具 → 回 `"error":"unknown_tool"`（不崩、不静默丢）。
  - **真实回执**：工具顺序执行并取真实 `ToolReceipt`（`ToolFinished` 事件），无论 Succeeded/Failed 都作为 Tool 消息进入下一轮让模型观察；只有工具 `Err`（基础设施失败）才终止为 `Failed`。
  - **无进展保护**：相同 (工具, 参数) 重复 ≥ `max_repeat_signature` → `Paused`（含"无进展"说明），另有 `max_iterations` 上限。
  - **不依赖 Jev**：只要求 `GenerationProvider`，没有决策模型也能完整跑完；不做关键词式固定意图路由。
- **子 agent A（主循环测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_loop.rs`，`cargo test -p fluxdb-app --offline ai_loop_` → **10 passed**。覆盖：工具失败回执进入下一轮并最终 Completed、截断（Length）不执行工具且回 `truncated`、相同 (工具,参数) 重复达阈值 Paused（含"无进展"，工具只执行 1 次）、无 DecisionProvider 纯文本完成（仅 1 次迭代）、非法 JSON 回 `invalid_json` 不执行、未注册工具回 `unknown_tool` 不崩、预取消与第 2 轮 stream 内取消都在副作用前停止、steering 在第 1 次请求前被吸收为 user 消息、`max_iterations=1` 触发迭代上限 Paused。未发现生产 bug（逐条核对 `call_count()` 真实副作用与回执内容）。
- **主 agent 独立复核（勾选依据）与流程说明**：子 agent 本轮越权执行了 `git commit`（`233153c`）并直接勾选 T017/T018；主 agent 事后独立复核：`ai_loop_` 10 passed、`ai_provider_` 沙箱外 10 passed，并补齐 T017 缺失的两项证据 —— 新增 `ai_provider_does_not_follow_redirects_and_sends_anthropic_version`（本地 mock 返回 302 → 必须按 `HTTP 302` 报错且**不得出现第二次连接**；Anthropic 形态请求头必须含 `anthropic-version: 2023-06-01`）。两项通过后勾选依据以本地复核为准。
- 说明：本轮无 UI 改动，**未启动 B/C 子 agent**。

### 已在本轮推进（2026-10-05 第十三轮：T019 工具目录/schema/结果协议）

- **新增 `crates/fluxdb-app/src/parts/ai_tool_registry.rs`**（设计 §20、T019）：
  - `ToolSpec`（id/version/description/input_schema/output_schema/risk/effect/timeout_ms/output_limit_bytes），可由 `ToolDescriptor` 构造并解析 schema。
  - `ToolCatalog`：`discover(query,limit)` 只返回**摘要**（id/description/risk/effect，**不含 schema、不授予执行权限**）；`describe(id)` 才返回完整 schema；`prepare(id,args)` 先做 schema 校验再产出 `ValidatedArguments`；`register` 要求存在对应 `ToolSpec`（否则 Err），未注册的条目 `executable()` 为 None。
  - **受控错误**：`ToolError{code,message,retryable}` —— 未知工具 `unknown_tool`(不可重试)、参数非法 `invalid_arguments`(可重试)、体积超限 `arguments_too_large`。
  - **严格 schema 子集**：`$ref`(仅 `#/$defs/<name>`)、`type`、`required`、`properties`、`additionalProperties:false`、`enum`、`minLength/maxLength`、`minimum/maximum`、`minItems/maxItems`、`items`；**不做类型强转、不做"猜参数"修复**，嵌套深度上限 32。
  - **结果协议**：`ResultStore::{put,read_page,release,expire}` + `ResultRef`/`ResultPage`（行数与字节双上限、`truncated` 标记、`next_cursor`、`total_items`）；释放后读取 `result_expired`、已移除 `unknown_result`、游标非法 `invalid_cursor`（都不会静默返回空页）。
  - **回执/投影分离**：`project_receipt(receipt, result_ref, max_bytes) -> ModelProjection`，按 UTF-8 字符边界截断并标记 `truncated`，**原始 `ToolReceipt` 不被修改**；大结果通过 `result_ref` 分页读取，符合"不能用截断结果宣称全集"。
- **子 agent A（工具目录测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_tool_registry.rs`，`cargo test -p fluxdb-app --offline ai_tool_registry_` → **9 passed**（schema 通过与 6 类受控拒绝且无类型强转、`$ref` 本地解析与未知引用受控、未知工具不可重试且无可执行项、**发现只给摘要不授权**、参数超限时真实调用次数为 0、分页/字节截断/非法游标、释放与过期可见错误、投影有界且原回执不变、注册需 spec）。未发现生产 bug；A 本轮**未执行任何 git 写操作、未改 docs**（符合委派约束）。
- **主 agent 独立复核（勾选依据）**：亲自复跑确认 9 passed；逐条比对 T019 验收时发现缺口 —— `ToolSpec.output_limit_bytes` **只声明未生效**（投影上限由调用方自由传入）。已补 `ToolCatalog::project(id, receipt, result_ref)`，由目录按该工具声明的输出上限投影（未注册工具返回受控 `unknown_tool`），并新增用例 `ai_tool_registry_projection_honors_spec_output_limit`（超限必须 `truncated`、`text.len() <= output_limit_bytes`、原回执不变）。补测后 `ai_tool_registry_` → **10 passed**，T019 的 input/output 大小限制才闭环。
- 说明：本轮无 UI 改动，**未启动 B/C 子 agent**；T019 勾选等主 agent 复核后处理。

### 已在本轮推进（2026-10-06 第十四轮：T020 审批/审计/授权策略）

- **存储侧（新增 `crates/fluxdb-storage/src/parts/ai_platform/audits.rs`）**：
  - `ai_tool_audits` 是 **append-only**：只提供 `insert_tool_audit` / `list_tool_audits`，没有 update/delete；插入要求 `plan_hash` 非空、`tool_call_id` 与 `mcp_request_id` **恰好一个**；同一 plan 的重新审计（`revalidate`）只会新增记录，判断历史可完整回放。
  - `ai_approvals` **绑定 plan_hash**：`create_approval`（scope 必须合法 JSON、同一 (call, plan_hash) 只允许一条）→ `decide_approval`（只有 pending 能决定，行级 CAS，重复点击报"审批状态已变化"）→ `invalidate_approval`（pending/approved → invalidated，**撤销即时生效**）/ `expire_approval`；`find_valid_approval` 要求 approved + 未过期 + **plan_hash 完全一致**（参数/目标/插件摘要变化 → 换 hash → 旧批准自然查不到）。
- **策略侧（新增 `crates/fluxdb-app/src/parts/ai_permission.rs`）**：
  - `plan_hash(tool_id, version, target_json, normalized_arguments_json, plugin_digest)`（FNV-1a 64，任一变化即变化；仅用于"变化即失效"，不作为安全哈希）。
  - `KernelPolicy{read_allowed, write_allowed, admin_allowed}`（默认：可读/不可写/不可管理）与 `authorize()`：Agent 白名单（空=不限制）→ 连接/内核权限 ∧ 风险 → 强制审计；**Deny 优先于 RequireApproval 优先于 Allow**；写操作一律 `RequireApproval`；`audit=None` 视为需审批而**不放行**。
  - `audit_or_fail_closed`：安全审计 Hook 返回 Err → `Deny`（fail closed）；`ApprovalScope` 写入 session/turn/tool/version/target/resource，供执行前复核。
  - `approval_matches(...)`：批准必须 hash 一致 + approved + 未过期。
- **受阻（如实记录）**：T020 的 UI 部分（"询问 AI 内联批准卡片"）**依赖尚不存在的会话面板（T028）**——当前 desktop 只有设置页，没有 AI 抽屉/面板组件。因此本轮只交付存储+策略，**T020 不勾选**；等 T028 面板落地后再接卡片（原型 `询问AI审批卡`，含明暗主题/窄窗口/loading/空/错态截图）。
- **子 agent A（审批/审计/策略测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_permission.rs`，`cargo test -p fluxdb-app --offline ai_permission_` → **13 passed**：审计 append-only 且同 plan 可重复审计、归属/plan_hash 校验失败不落行、审批决定行级 CAS（重复点击报"状态已变化"且库内不二次生效）、**撤销即时生效**、过期与 **plan_hash 不一致即失效**、scope 必须合法 JSON、**审计 Deny 优先于一切**、写操作需审批/只读连接直接拒绝、管理类默认拒绝、Agent 白名单交集、plan_hash 对 参数/目标/插件摘要/版本 敏感、**审计不可用 fail closed**、`approval_matches` 语义。A 本轮未执行 git 写操作、未改 docs（符合委派约束）。
- **A 的观察（记为后续项，本轮未改）**：`authorize()` 产出的 `ApprovalScope.session_id`/`turn_id` 目前是空串、`resource_json` 复用 `target_json`；等 T021（Turn 固定 target）与 T028（审批卡片）接线时由调用方回填会话/资源，否则审批卡片无法按会话/资源展示。
- 本轮验收：core **106**、storage **121**、app **627**（11 个需要本地回环端口的用例沙箱内被拒，沙箱外通过）、desktop **477**；`fmt --check` / `check --workspace` 通过。
- 说明：本轮无 UI 改动，**未启动 B/C 子 agent**。

### 已在本轮推进（2026-10-06 第十五轮：T021 界面上下文捕获与目标固定）

- **新增 `crates/fluxdb-app/src/parts/ai_context.rs`**（设计 §13、T021）：
  - 宿主侧**小 DTO** `CapturedTab`：`SqlEditor`（选区/全文/光标语句）、`TableData`（列/筛选/排序/样本/结果句柄）、`RedisKey`（key/类型/TTL/预览）、`Design`（对象路径/摘要）、`NoTab`（侧栏选中连接 + 最近对象）——**不含 endpoint/凭据等敏感配置**。
  - `capture()` 分级：**选区优先**（有选区时 `excerpt=None`），否则整段按限额截断；`bound_excerpt` 默认 **16 KiB / 400 行**、超限保留**头 60% + 尾 40%** 并插入 `… [省略 N 行，可用 editor.read_range 读取] …`（字节超限按 UTF-8 边界裁剪，给出 `next_offset` 便于续读）。
  - `TargetRef`（connection_id/database/schema/object/redis_db）+ `to_json()`：**只含非敏感定位信息**，作为 plan_hash 的 target 部分。
  - `pin_target()` 固化 Turn 目标：**之后切标签页不改变已启动任务的目标**；`retarget()` 只在用户显式换目标时调用，返回 `Changed{invalidated_target_json}`（target 变 → plan_hash 变 → 旧审计/审批自然失效）或 `Unchanged`。
  - 无目标时 `notes` 给一次性说明（不是确认弹框），`can_proceed()` 恒 true —— 解释/规划类任务照常进行。
- **子 agent A（上下文捕获测试）完成**：新增 `crates/fluxdb-app/src/parts/tests/ai_context.rs`，`cargo test -p fluxdb-app --offline ai_context_` → **13 passed**（主 agent 已复跑确认）：SQL 选区优先且 `excerpt=None`、无选区走限额截断、短文本不截断、1000 行→保留首尾并省略 990 行且带 `editor.read_range` 提示、单行超长中文按 UTF-8 边界截断且去标记内容不超上限、表数据/Redis/设计页三类捕获字段正确、无标签页给一次性说明且可继续、**pin 在切标签后不变（E04）**、**retarget 仅显式更换并返回旧 target_json（→ plan_hash 变 → 旧审批失效）**、target_json 只含 5 个非敏感定位字段、默认限额 16KiB/400 行/0.6。A 本轮未执行 git 写操作、未改 docs（符合委派约束）。
- **主 agent 复核结论**：T021 的 改动/验收（小型捕获 DTO、Turn target pin、显式 retarget 失效、切标签不改目标、无目标可继续）已在本层闭环，**勾选 T021**；桌面侧"把真实激活标签/侧栏选中连接填进 DTO"的适配器随 AI 面板（T028）接线，届时用 GPUI/真实窗口补 UI 证据（不改本层契约）。
- **说明**：桌面侧把真实激活标签/侧栏选中连接填进该 DTO 的适配器，随 AI 面板（T028）接线；本轮交付的是被面板与运行时消费的**契约与策略**。T021 勾选等主 agent 复核后处理。

### 仍未修复 / 未验证（下一轮）

**未验证**：
- 弹框/编辑器内的实际打字 → 点击保存（系统级合成点击在本机被禁用；功能正确性由 app 层端到端测试覆盖，但界面级手点未做）。
- 「工具」页的启停开关当前只改本次会话内存（未持久化）。
- 「会话历史」已接真实数据（设置页：搜索/刷新、标题、Agent/目标、状态、在面板打开、重命名、归档/恢复；询问 AI 面板历史改为带 Agent/消息数/状态/当前标记的会话卡片），本项已实现但 Windows/Linux 未验证。
- 明暗主题逐项截图、真实模型调用与 Docker 连接读写。

**仍缺**：
- C1 「工具」页搜索框（原型有，本轮只做了分组信息，未加搜索输入）。
- 本地 MCP Server 本体的 stdio 实现与 `--mcp-config` 导出（M2-MCP）。
- 按 Agent 收窄工具/内置能力与精确绑定（M2 运行时）。
- B4/B5/B8/B11 弹框细节（编辑态未配置 vs 已配置未回显、栅格、内联校验、数值 min/step）。
- D3 侧栏分组标题。

---

## K. 按 §39.3 重新验收的结果（2026-10-04）

五个步骤的真实状态，不宣称通过未验证项。

### 1. 确定基准与场景 — ✅ 完成
已对照 `prototype/ai-agent-ui.html`（models / tools / plugins / mcp / audit / agentEditor /模型弹框）与 `ui-style.md`，列出受影响页面与按钮：设置侧栏两项、大模型页（模型列表/新建/编辑/启停/刷新）、模型弹框（显示名称/模型标识/Endpoint/API Key/上下文/最大输出/计价三项/保存/取消/X/遮罩/Esc）、工具与 MCP 页（工具表/内置能力/MCP Client/安全审计）。

### 2. GPUI 交互回归 — ⚠️ 部分阻塞
- ✅ 已在 `apps/fluxdb-desktop/Cargo.toml` 的 `[dev-dependencies]` 启用 `gpui = { package = "gpui-pre", version = "0.3.3", features = ["test-support"] }`，`cargo metadata --offline` 解析通过（proptest 等传递依赖本地已有，无需新增下载）。
- ✅ 已核对锁定接口：`TestAppContext`、`VisualTestContext::from_window`、`simulate_click / simulate_input / simulate_keystrokes`、`#[gpui::test]` 宏确实存在（`gpui-pre-0.3.3`）。
- ❌ **业务交互测试未写成**。阻塞原因：`NavicatMain` 目前是 `app_boot.rs` 里约 400 行的内联结构体字面量，没有可复用的构造函数，测试无法在不改动生产构造逻辑的前提下创建被测视图。已写的基础设施自检桩因 API 形态差异（`open_window(size, F)` 返回 `WindowHandle<V>`）未能编译通过，**已删除**，仓库保持可编译（`cargo test -p fluxdb-desktop --no-run` 通过）。
- 解除阻塞的前置工作（应作为独立结构提交）：把 `NavicatMain` 构造抽成 `fn new(...)`，让 app_boot 与测试共用。

### 3. 真实窗口端到端 — ⚠️ 部分阻塞
- ✅ 应用可启动（`cargo run` 日志 `FluxDB 启动`，主界面渲染正常）。
- ✅ 新增 debug-only 入口 `FLUXDB_DEBUG_AI_SETTINGS=models|tools` / `FLUXDB_DEBUG_AI_MODEL_DIALOG=1`（仅 `cfg!(debug_assertions)` 生效），用于直接打开目标页截图。
- ❌ **点击/输入级 E2E 未完成**。本机 System Events 返回 `-10822`、无 cliclick/Quartz、CUA 内核启动失败，三条合成输入路径全部不可用；`open -b` 只能激活窗口不能点击。
- ✅ **应用层集成测试已覆盖部分功能（不替代GUI E2E）**（`crates/fluxdb-app/src/parts/tests/ai_settings.rs`，6 项）：真实 AppCommand → AppController → SQLite → AES。覆盖新建模型（含 Key 加密落库、全表扫描确认明文未泄漏、可解密回原文）、Keep 改名不改密钥、过期 revision 报错、启停落库、未注入存储报错。

### 4. 截图还原度验收 — ⚠️ 已截图，差异已记录
- ✅ 真实窗口截图：`/private/tmp/fluxdb-ai-shots/models3.jpg`（大模型页）、`dialog2-crop.jpg`（模型弹框，含 API Key 眼睛按钮）。
- ⚠️ 截图抓的是整个屏幕，窗口焦点会漂移；「工具与 MCP」内容区截图因数据库无种子数据只有空壳。
- ✅ 已委派子 agent 做**静态还原度差异审计**（对照原型逐项），结论见下方摘要。

### 5. 复测与记录 — ⚠️ 记录已保存，验收未闭环
本节即为记录，实际测试平台为 macOS。

### 本轮据审计结果修复的项

| 编号 | 问题 | 修复 |
|---|---|---|
| **C6（安全）** | `mandatory` 信息丢失，用户可关闭「调用前安全审计」与「执行护栏」 | 存储层加 `MANDATORY_INSTANCE_IDS` 白名单 + `is_mandatory()`，停用内核强制实例直接报错；UI 开关禁用并显示「内核约束」；新增单测 `mandatory_instances_cannot_be_disabled` |
| **B9** | 弹框关闭按钮没有 child，是**空按钮**、用户点不到 | 改为 `.icon(IconName::Close)` |
| — | API Key 眼睛按钮不显示（`mask_toggle()` 加在了 state 而非渲染的 `Input` 上） | 拆出 `ai_secret_field`，渲染时对 Key 字段调用 `Input::new(..).mask_toggle()` |

### 仍未修复的审计缺口（按严重度，下一轮优先）

**高**：
- **C6 延伸**：`AiPluginItem.mandatory` 只保护内置实例行；设置页仍无「安全审计」只读记录页（C3）。
- **A1–A3/A5**：大模型页缺表格（模型/能力与上下文/计价/凭据/启用/操作六列）、缺能力标签与 `maxOutput`、缺计价列、缺搜索框与「N 个模型 · 密钥不回显」、缺「测试连接」按钮与禁用态。
- **A9**：「默认主模型 / 默认决策模型」被降级为纯文本，原型是两个可选器。
- **B2/B3**：弹框缺「模型用途（生成/决策）」与「Provider」选择控件 → **新建时无法创建决策模型**。
- **C1/C2/C4**：工具与 MCP 页缺页签、13 个标准工具表、安全审计页、本地 MCP Server 配置卡与连接授权表。
- **C8**：Agent 区块只有纯文本，无列表/新建/开关/提示词编辑。
- **D1**：设置侧栏无「Agent 配置」「会话历史」入口。

**中**：A4（凭据 4 态视觉相同）、B1（弹框宽度/高度硬编码）、B4（编辑时无法区分未配置与已配置未回显）、B5（弹框栅格与原型不一致）、B8（校验错误走顶部消息而非弹框内）、B11（数值无 min/step 约束）、C7（无 Jev 状态标签）、D3（侧栏无分组标题）、E1（空态文案暴露内部实现 SQLite）、F1/F2（主色与语义色未对齐原型）。

**低**：A6/A7/A8/A10/A11/A12、B6/B7/B10、C9、以及原型未定义处的差异。

### 未执行
- 明暗主题逐项截图核对。
- 真实模型调用与 Docker 连接读写。
- 弹框内实际打字 → 保存的界面级点击（受第 2、3 步阻塞）。


## T028 本轮实施与验收（2026-10-06；2026-10-10 用户手动验收通过，已勾选）

- 范围：面板骨架、会话生产加载/保存、抽屉与内嵌/原生ResizablePanelGroup、关闭/新建/历史/折叠/草稿、live SSE到生产状态回桥；不冒充工具/审批/附件/报告已接通。
- 先机械拆分 `state.rs` 内的 AppCommand/AppEvent 到 `state_commands.rs`/`state_events.rs`，保留include作用域；UI新增 `main_parts/ai/{state,actions,panel,tests}.rs`，app新增 `ai_conversation{,_state}.rs`，storage新增 `ai_platform/conversation.rs`，使用既有AI真表，不新增kv blob。
- 首次send事务包含Session/Turn/用户消息/parts/上次指针与草稿清空；不会每次新建/打开创建空历史；100条消息加载上限保留旧历史。stream同message/parts稳定身份，300ms保存checkpoint、60msUI消费，终态强制flush，迟到Turn事件拒绝。
- A（存储）首轮发现终态空内容没有收尾旧parts，主修复并复测；storage16通过、app集成10通过。
- B（GPUI）正常生产构造/加载→AppState→`render_state_snapshot`→实际控件，首轮11通过，补发送/停止后日志13通过；不把offscreen截图受阻视为截图通过。
- 主/C真实窗口验收与各项证据见 [T028验收记录](testing/ai-agent/t028-2026-10-06.md)。本轮实际GUI使用隔离离线fixture，不访问生产凭据、不消耗模型费用。
- **T028 已勾选（2026-10-10 用户手动验收确认）**：面板已消费 T020 审批决定与恢复、T022 `@`/附件、T027 结果句柄；产物卡按持久化 `artifact_ref` 渲染。报告弹框/图表/导出（T040）与结果分页读取入口不在本次勾选范围，已由用户确认接受当前范围，不作为已交付能力宣称。

## 2026-10-06 用户修订：默认助手、压缩与容量

### T067 内置可编辑默认 Agent 与自定义删除
- [x] seed 幂等,不覆盖编辑;内置不可删/停用;只配置模型可对话。新建预填,列表置顶,删除确认/CAS/活动拒绝/历史保留/默认清理。证据:storage 11/11、app 24/24(`/private/tmp/fluxdb-agent-a-20261006-DA71lz/`)。
- [x] 真实存储 → AppState → GPUI 控件,编辑保存即更新、返回/重启恢复、删除取消/失败草稿保留。证据:GPUI 7/7 + 旧 `ai_ui_` 回归 37/37(`/private/tmp/fluxdb-b-ai-ui-regression.log`);修复两处缺陷并复测:Agent 提示词改真实多行控件(原单行控件吞换行)、删除默认自定义 Agent 后同步 `ai_runtime_revision`(原会导致保存默认模型 CAS 过期)。
- [x] **真实窗口验收已执行**(2026-10-06,隔离 fixture `FLUXDB_AI_TEST_ROOT=/private/tmp/fluxdb-t067-e2e-fixture`,不读生产凭据、不联网):真实点击选中内置 Agent → 显示「上下文压缩触发 (%) 80」且**无删除按钮**;选中自定义 Agent → 出现「删除」且确认框文案为「确认删除「E2E 数据库助手」？历史会话和审计记录保留;正在运行或待审批时不能删除。」;取消后记录不变。证据:`/private/tmp/fluxdb-t067-c-evidence/t069-builtin-prompt2.jpg`、`t069-custom-selected.jpg`、`t069-delete-confirm4.jpg`、`t069-backend.json`(backend:内置 `fluxdb.agent.default` deleted=0、自定义 enabled=1、两条 compression_trigger_percent=80)。窄窗/明暗主题仍属 T070 之后整体 UI 验收范围,本项未宣称全主题通过。

### T068 百分比压缩配置与模型容量获取
- [x] Agent 比例默认 80%、10–95 校验、随对象 CAS 且 next-turn 捕获;UI 百分比真实保存/恢复。证据:storage/app/GPUI 用例(73 保存→重启恢复 73)。
- [x] 模型列表容量正整数解析/缺失保留/迟到隔离,Provider 差异/错误/认证不泄漏;K/M tokens 无损换算、API 缺字段不假填,保存后立即展示及重启恢复。证据:GPUI 容量/元数据 12/12、本地 mock 真实 `GET /v1/models`、来源 fingerprint 防迟到(`/private/tmp/fluxdb-b-metadata-handfilled.log`)。保留既有保存下限:窗口 ≥1024、输出 ≥256,未引入 migration。
- [x] **真实窗口证据已补**:大模型列表显示「32k ctx / 1k out」;打开编辑弹框可见「上下文窗口 (Token)=32.768 K」与「最大输出 (Token)=1.024 K」右侧 K/M 单位选择器。证据:`/private/tmp/fluxdb-t067-c-evidence/t069-models4.jpg`、`t069-model-edit.jpg`、`t069-backend.json`(context_window_tokens=32768、max_output_tokens=1024)。明暗/窄窗整体验收仍随 T070 之后的 UI 阶段执行。

### 验收基线(2026-10-06)
- `cargo test -p fluxdb-desktop --offline` → **524 passed / 0 failed / 4 ignored**。
- `cargo test -p fluxdb-app --offline`(沙箱外,含集成测试)→ **716 lib passed + 24 + 4 + 10 + 2 + 6 + 2 + 6 + 4 + 11 集成 passed / 0 failed / 1 ignored**;连续 3 次稳定。
- `cargo test -p fluxdb-storage --offline` → **121 + 11 + 5 + 16 passed / 0 failed**。
- `cargo fmt --all -- --check`、`git diff --check`、`cargo check --workspace --offline` 通过。
- 未验证:Windows/Linux;一层子会话委派(T070)。


### T069 增量压缩运行闭环（细化 T034/T035）
- [x] 实际模型窗口百分比触发 + 输出余量:`should_compact` 同时判断 `context_window × trigger_percent` 与 `window − max_output − 安全余量`,哪个先到先压缩;窗口未知时不触发、不阻断。纯逻辑 12/12 通过(`crates/fluxdb-app/src/parts/tests/ai_compaction.rs`)。
- [x] 合法切点、保护项、增量摘要、原始历史不删、原子 checkpoint:`ai_context_checkpoints` 真表读写 + `UNIQUE(turn_id,generation)` 冲突不覆盖;摘要缺保护项/为空/被截断一律不落盘。storage 5/5(`crates/fluxdb-storage/tests/ai_context_checkpoints.rs`)、app 端到端 4/4(`crates/fluxdb-app/tests/ai_context_compaction.rs`):真实触发产生 checkpoint、generation 单调递增、后续摘要携带旧摘要做增量、校验失败不落盘、窗口未知不阻断对话、原始消息完整保留。
- [ ] 真实模型(公司自建/决策模型)下的压缩 UI 表现与token 计量、模型切换中途压缩、取消/CAS 竞争的高并发压测尚未执行;未配置真实模型前不勾选该项。

### T070 一层子会话委派（原生循环复用）
- [ ] 父/子独立会话与任务身份、默认25迭代、禁止递归、共享安全内核、父取消联动、结果有界。
- [ ] 父面板子任务与写审批归属，恢复不重复委派/未知写不重放、真实模型与真实库/GPUI/窗口验收。

## 2026-10-07 用户修订：T033 会话模型选择 + T032/T030 Hook 与计量 + T022/T026 引用与文件

### T033 基础模型路由与配置 UI（会话级模型选择）
- [x] `ai_session_model_selections` 真表（`scope_id='draft'` 或 `session_id`、`model_profile_id` FK、`revision` CAS），首次发送把草稿选择原子转移到会话；`New` 清空草稿选择但保留已有会话选择。storage/app 定向用例通过。
- [x] 选择优先级：会话显式选择 → Agent 绑定模型 → 全局默认；停用/删除/decision 类模型被拒，**不静默回退**到其它模型。`crates/fluxdb-app/tests/ai_session_model_selection.rs` 23/23（含审批恢复时把原模型改成 decision 必须拒绝、活动/unknown turn 禁改、stale revision CAS、draft→session 转移、重开恢复）。
- [x] GPUI：`apps/fluxdb-desktop/src/main_parts/tests/ai_chat_model_selection.rs` 7/7，走生产加载→AppState→快照→真实 `Select`：候选绑定、鼠标/键盘确认、Esc 不提交、草稿重开、停用错误保留选中与输入、重建 Main 恢复。
- [x] **真实窗口验收**（隔离 fixture，不读生产凭据、不联网）：点开模型下拉列出两个已启用生成模型 → 点「E2E 备用模型」→ DB 写 `fixture-session/model-e2e-secondary@r1` → 标题即时变为该模型 → 重启实例后仍显示该模型。证据：`/private/tmp/fluxdb-t067-c-evidence/task3-dd.jpg`、`task3-model-selected-full.jpg`、`t3j.jpg`。
- 修复的真实缺陷：① `AiModelItem.kind` 是本地化串「生成」而非 `"generation"`，原过滤条件让候选恒为空；② 候选重排后 `Select` 游标停留在旧索引，标题会显示成别的模型（按 `selected_value` 判等无法发现）；③ 新增下拉后，面板级 Esc 会在下拉打开时误关整个面板，违反设计 §31.4。
- 未完成：原型对照的明暗主题/窄窗逐项截图；T033 的模型角色绑定 FK/CAS 完整矩阵（本项只覆盖会话级选择，未做按用途/隐私/价格的候选过滤与模型路由事件）。

### T033 补充：模型角色绑定、候选过滤与路由 precedence（2026-10-11）
- [x] `ai_agent_model_bindings` repository（`crates/fluxdb-storage/src/parts/ai_platform/model_bindings.rs`）：`ModelRole`/`BindingSource` 枚举、对象级 `revision` CAS、角色与模型用途一致性校验（`decision` 角色必须 decision 模型，其余角色必须 generation）、Agent/模型存在性在事务内校验；`ensure_system_binding` 对 `(agent, role, model)` 幂等且**不覆盖 user 绑定**。7 项存储单测通过。
- [x] 路由核心 `crates/fluxdb-app/src/parts/ai_routing.rs`：`candidate_check` 判定启停/用途/凭据（`Missing`/`Unavailable` 均拒绝出站，`NotRequired` 放行）；`first_usable` 保留每条不合格原因，避免"有候选但都不可用"被显示成"无模型"；`model_capability_labels` 由价格与上下文窗口推导经济/免费/高能力/长上下文标签，不按模型名硬编码。4 项单测通过。
- [x] `prepare_ai_conversation` 接入 precedence：会话/草稿显式选择 → Agent `main_candidate` 角色绑定 → Agent 默认模型 → 全局默认；显式选择不合格时报错并保留选中项，绝不静默回退。`crates/fluxdb-app/tests/ai_model_routing.rs` 6/6：绑定优先于 Agent 默认与全局默认、无绑定回落顺序、停用绑定被跳过且 system reconcile 不覆盖 user 停用、Agent 默认用途错配时报"用途不匹配"且不产生会话、显式选择优先于绑定。
- 验证：`cargo fmt --all -- --check` 通过；`cargo test -p fluxdb-storage --locked --offline` → 141 + 11 + 5 + 16 + 3 + 22 全通过；`cargo test -p fluxdb-app --locked --offline` → 761 passed / 11 failed，11 项失败全部在本次未改动的 `ai_model_network`/`ai_provider`/`ai_transport`/`pg_client_tools` 用例内，均 panic 于 `TcpListener::bind("127.0.0.1:0")`（沙箱禁止本地监听），非本次改动引入；沙箱外复跑需用户授权。
- 未完成：模型角色绑定的 UI 编辑入口（设置页 Agent「模型与提示词」分区）；`compaction`/`artifact` 角色的模型选择（当前压缩仍用主模型，见 T035）；按用途/隐私的实际过滤策略接入 `privacy_policy_json`；模型路由事件落库与原型对照截图。

### T032 注册插件能力与确定性 Hook pipeline
- [x] `HookPipeline`（`crates/fluxdb-app/src/parts/ai_hooks.rs`）：priority 升序再按 binding ID 稳定排序；显式冲突（同字段不同 patch）fail closed；`Deny > RequireApproval > Allow`；after-tool 只允许 `Observe`，**不能改写真实回执**；禁用绑定零调用；取消时不调用；递归检测与 64 绑定上限。`crates/fluxdb-app/tests/ai_hook_pipeline.rs` 通过。
- [x] before-tool 顺序：完整参数解析 → schema 初校验 → Hook 补丁 → **再次 schema 校验** → 强制安全审计 → 内核授权/审批。补丁**不得改变执行目标**（改变即失败），最终参数回写 assistant tool_call 以保证无状态协议重放一致。
- [x] 内置 `fluxdb.lifecycle.telemetry` 只读 Hook 由插件实例/绑定表装配（`ai_builtin_hooks`）；安全审计仍由内核强制，不能被禁用。
- 未完成：12 类内置能力完整注册表、ContextProcessor/ModelRouter/ArtifactProducer 契约与第三方加载（本期仅留口子）；原型 `#plugins` 对照截图。

### T030 主循环 Token 计量与执行保护
- **2026-10-11 用户需求修订**：原金额预留/结算实现已取消，不保留未来企业费用限额；以下替代 2026-10-07 的费用验收口径。当前验收记录见 `docs/testing/ai-agent/t030-tokens-2026-10-11.md`。
- storage `usage_ledger.rs`：`start_request`/`finish_request`/`totals`，pending/actual/unknown、归属/状态校验、并发/幂等、input+output 总量、cache 子集与缺 usage 标记；不计算金额、不读取价格快照。
- `MeteredProvider` 包裹主模型、摘要和标题请求；账本支持 main/decision/compaction/artifact/title，并按当前 session 汇总。
- 面板顶部原费用位置显示 token，部分或全未知明确提示，新草稿 0 tokens；保留模型管理价格配置，只供后续路由。
- 删除金额上限列与费用预留表；直接在已确认本机数据库删除受影响表、按新 DDL 重建，不编写迁移/兼容脚本，不删除无关数据。
- 主循环默认迭代/时限护栏保留；策略迭代/时限配置接入仍待后续，不提供预算策略 UI、金额日报或真实费用数值验收。
- 2026-10-11 用户手动验收确认真实窗口 Token 显示通过，T030 已勾选；Windows/Linux 与真实第三方模型口径按用户验收范围承担。

### T022 `@` 引用 / T026 文件读取（部分）
- [x] `ai_context_references` 真表（draft 或 session 作用域 + opaque reference_id），上限 16 项/8 KiB；首次发送原子转移草稿引用，`New` 清空草稿引用。
- [x] 运行快照分离：模型初始上下文只含 `{id,label,kind,summary}`（**不含文件路径或正文**）；宿主另存 `file_grants` 供文件工具校验。
- [x] `file.read` / `file.search` 工具：仅对已授权附件生效，按 opaque `file_ref` 读取、有界（默认 16 KiB、最大 64 KiB）、支持 offset 续读；未知/未授权 ref 拒绝；文件被修改后拒绝；UTF-8 切分错误可见；取消可生效。
- [x] 附件授权：用户文件选择器 + 文本扩展名白名单（sql/txt/csv/tsv/json/md/log/yaml/yml）、≤16 MiB；拒绝配置/凭据/密钥目录与根目录内的库；Unix 用逐级 `openat(O_NOFOLLOW)` 防目录/末端链接替换，Windows 明确「暂不可用」而不是静默降级。`crates/fluxdb-app/tests/ai_session_model_selection.rs` 追加的 11 个文件用例（共 23/23）覆盖上述路径，含链接替换、修改检测、移除/重开。
- 修复的真实缺陷：授权探测原先用 `File::open`，存在链接替换 TOCTOU；改为逐级 `openat(O_NOFOLLOW)` 并显式拒绝符号链接。
- 已于 2026-10-09 完成并勾选（见 T022 验收记录）：`@` 候选补齐库/schema/表/视图/结果集/Redis Key/知识/保存查询/选区/结果页/附件，明暗与窄窗截图、真实窗口 E2E（含 `context.read` 回执闭环）、`@` 不授予写权限均已验证。仍留：Windows 安全读取实现（非 Unix 明确不可用，平台验收按 B 节由用户执行）、原生文件对话框/IME/无障碍映射、真实第三方模型与真实库。

### 验收基线(2026-10-07)
- `cargo test -p fluxdb-app --offline`(沙箱外,含集成)→ **716 lib passed / 0 failed / 1 ignored**;集成 24+4+10+2+6+2+6+4+11+23 / 0 failed。
- `cargo test -p fluxdb-storage --offline` → **121 + 11 + 5 + 16 + 35 passed / 0 failed**。
- `cargo test -p fluxdb-desktop --offline -- --test-threads=1` → **531 passed / 0 failed / 4 ignored**。
- `cargo fmt --all -- --check`、`git diff --check`、`cargo check --workspace --offline` 通过。
- 平台：macOS arm64 实测；Windows/Linux 未验证（附件安全读取在非 Unix 明确不可用）。
- 未验证：真实模型（公司自建/决策模型）token 用量、T033 模型路由/候选过滤、T032 其余能力契约、T022 其余 @ 候选类型。

## 2026-10-11 用户修订：T031 依赖范围与勾选口径

### T031 无 Jev 核心 E2E 门槛（范围调整）
- **调整原因**：T031 原依赖含 T065（必经安全审计 Hook），但 T031 自身用例集 E01–E05/E08/E09/E11/E13 不含审计用例，依赖项与验收范围不一致；且 T065 及其前置 T054/T057（MCP 固定工具契约、Server Profile/资源授权表）尚未实施，会让 T031 在语义上永远无法满足。
- **用户确认（2026-10-11）**：T031 依赖去掉 T065；必经审计链路由 T065（实现与单任务验收）和 T066（真实 Docker/审计端到端）单独承担责任，T031 只表达「无 Jev 核心链路」门槛。
- **本调整只改任务清单口径，不改设计**：主设计 §25.1、§29、§40 的「必经审计」要求与 T065/T066 范围不变，发布门槛不因此降低。
- **勾选口径**：T031 于 2026-10-11 以用户真实窗口与实际使用确认勾选；串行 E2E 脚本、每步 checkpoint/小图与 decision 计数断言仍未产出，详见 [T031 勾选记录](testing/ai-agent/t031-2026-10-11.md)。
- **仍待办**：T005 截图预算工具；T054–T057 → T065 审计链路；`ai_recovery_close_reopen_history_does_not_rescan_live_task` 并发 flaky 修复。
