# fluxDB 原生 AI Agent：实施任务清单与验收

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
7. 所有插件只能在内核权限/预算内运行；不能伪造工具成功、跳过审批。
8. 每个 UI E2E 步骤只看当前小图；先压缩本地图片，再输入视觉上下文，每步保存文本 checkpoint。
9. Rust 变更 cargo fmt/check；业务变更相关测试；UI 变更启动主窗口和实际交互；三端结果分开报告。
10. 所有框架引入先锁版本/查许可证/验证，不能根据main文档猜发布版功能。
11. 本期只实现内置插件/自动可选Jev，第三方只留接口，不写安装器/Wasm。无历史AI数据，不做迁移引擎。
12. 真实DB/UI/MCP测试用用户已授权的现有Docker MySQL/PostgreSQL/Redis连接；按本run namespace隔离，设置“大模型/工具与MCP”菜单和持久时间线为验收重点。

## B. 任务记录规范

每个 Txxx 结束记录：改动文件、实际命令与结果、平台、用例/step、证据和 checkpoint 路径、未验证范围。任务勾选仅在验收真实通过后，不因代码写完就勾选。

- P0：核心闭环与安全/数据正确性，不能跳过。
- P1：用户所需智能优化、Hook/BI/插件体验。
- P2：可安装插件、MCP 与新能力扩展。
- 依赖标记 `Txxx`；并列任务可以在互不重叠写集下推进，但本清单不自动授权子 Agent。
- 初始代码落点按设计 §7；具体文件名可小幅调整，但职责/可见性/调用方向不改变。

## C. 文档阶段产物

- [x] D001 汇总用户需求、内嵌 Agent 和非目标；设计 §1–3。
- [x] D002 核对现有执行/存储/加密调用链及本地 Pi/Goose commit；设计 §4–5。
- [x] D003 写出模型/插件/工具/产物/运行时契约、完整独立表 DDL；设计 §6–36、§42。
- [x] D004 旧基线27表/31项、MCP增补31表/44项保留为历史；本轮删除迁移并新增parts/UI/audit，33表/65项通过，全部是文档/内存SQLite检查。
- [x] D005 按八点需求修订内置插件/自动Jev、菜单、美观时间线、消息parts、必经审计、现有Docker测试与本期/延期边界；所有实现细节/就近参考统一并入主设计对应章节。
- [x] D006 两份专项设计完整合并主文对应章节，任务引用同步；旧文件仅作跳转索引。DDL字节不变，33表/70项文档与内存SQLite检查通过。

## M0：边界和原型（P0）

### T001 旧文件职责拆分
- [ ] 依赖：无；设计 §4、§7。
- 改动：AppState/AppCommand/AppEvent 与 storage/UI 超长文件先做机械拆分，保留 include 作用域和可见性；新功能落独立 ai 目录。
- 验收：结构改动不改变现有行为；cargo fmt/check 和原有相关测试；diff 中不夹带无关重构。

### T002 建立领域 ID/DTO/错误和应用能力边界
- [ ] 依赖：T001；设计 §8–12、§20。
- 改动：core AI DTO；app Provider/Tool/Repository/Hook 契约；明确 SecretInput 的不可序列化与脱敏 Debug。
- 验收：core 不依赖 GPUI/HTTP/具体驱动；trait 对象安全；无新层级循环依赖；DTO 序列化白名单测试。

### T003 模型基础库与流式 transport 原型
- [ ] 依赖：T002；设计 §6、§17。
- 改动：验证现有依赖、Rig 可步进循环/Provider 与自有策略的组合，优先复用可用基础能力；固定具体版本，选定 HTTP/SSE、cancel 和 schema 依赖。
- 验收：mock server 能返回分片/工具参数/usage/截断/错误；取消可中断等待；body/frame 有界；不引入外部 Agent 进程。
- 记录：所选 commit/版本、许可证、三端依赖和包体；若不采用 Rig，解释实测原因，而不是全部从零重写的口号。

### T004 建立 scripted Provider 与隔离 fixture
- [ ] 依赖：T002；设计 §37。
- 改动：app tests/scripted Provider，可按调用接收真实 tool receipt 并继续；隔离 storage root、测试 SQLite/文件 fixture。
- 验收：不读取生产配置/真实 Key、不使用付费网络；脚本不绕过工具执行直接伪造数据库结果；正常 UI 命令能接入该 Provider。

### T005 三端 UI E2E 入口与截图预算工具
- [ ] 依赖：T004；设计 §38。
- 改动：测试专用启动/fixture 配置、每步语义断言和小图处理；先验证捕获/编码工具及平台可用性。
- 验收：raw 图不进入视觉上下文；≤1280×800、≤1MP、≤512KiB、目标≤250KiB；文字可读；每步 checkpoint；debug/test 注入不进生产入口。

## M1：独立管理表、凭据与配置界面（P0）

### T006 首次创建 AI schema 与内置种子
- [ ] 依赖：T002；设计 §35–36、§42。
- 改动：[schema.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/ai_platform/schema.rs)和init；SQLite open 的FK/busy timeout；不新增ai_schema_migrations/旧AI转换。
- 验收：首次建表、重复打开、事务中断、同名结构不匹配明确错误；DDL 唯一来源；不修改已有业务 kv/连接/history 内容。

### T007 提取共享 AES/key loader 并兼容实际密钥字段
- [ ] 依赖：T006；设计 §33。
- 改动：[secret_crypto.rs](/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-storage/src/parts/secret_crypto.rs)，连接后端转调用；兼容 secret_key/connection_secret_key、保留旧 AAD。
- 验收：现有连接密码全部可读；同名冲突显式失败；设置保存不丢密钥；新安装随机密钥原子初始化；旧内置钥匙不被偷偷替换。

### T008 模型 private encrypted row 与凭据状态
- [ ] 依赖：T007；设计 §33–34、§42.3。
- 改动：模型 ciphertext/nonce/cipher_version/key_id/secret_revision、AAD；公共 DTO 与私有行分离、Provider 专用 lease。
- 验收：不同模型交换密文/nonce/secret_revision 导致认证失败；12-byte nonce；同 Key 多次 Set 不复用 nonce；未知版本/损坏/错钥匙为 Unavailable；系统凭据调用数=0。

### T009 模型 Keep/Set/Clear 与 CAS 原子保存
- [ ] 依赖：T008；设计 §34。
- 改动：专用保存 API，同事务配置+秘密+revision；commit/磁盘错误注入。
- 验收：Keep 秘密列字节不变且不因密钥不可用阻断非敏感编辑；Set/Clear 正确递增；两编辑器冲突；失败保留旧 Key/配置；Debug/错误日志无敏感输入。

### T010 模型/预算/运行设置独立 repository
- [ ] 依赖：T006、T009；设计 §17、§30、§42.10。
- 改动：分页读取、逐对象 CRUD/CAS、能力/价格/endpoint/非敏感 JSON 校验；不新增 save_all_ai_config。
- 验收：未知价格不是免费；删除引用模型被阻止；禁用在下次调用生效；export 列白名单不含加密列；导入需补 Key。

### T011 Agent直接提示词与运行快照
- [ ] 依赖：T006；设计§19、§31.8、§42.10。
- 改动：ai_agents.system_prompt直接输入/保存/对象CAS，Turn捕获正文/hash与Agent revision；无模板/版本/发布表或页面。
- 验收：两Agent提示词互不覆盖；编辑只影响下个Turn；旧快照保持；UTF-8长度/空值安全基线；日志不输出全文，不因提示词放宽权限。

### T012 Agent、插件 package/instance/binding 与 MCP 管理
- [ ] 依赖：T010、T011；设计 §23–25、§42.10。
- 改动：逐实体save；内置Registry/system实例和绑定自动建立、default decision引用、system/user角色优先级；模型角色/MCP授权独立绑定；当前不支持第三方加载。
- 验收：每实体 revision 独立；失效引用/重复 binding/不兼容 schema 拒绝；Jev binding 不允许 required；无 Jev seed 可用。

### T013 模型设置 UI
- [ ] 依赖：T005、T009–T010；设计 §31–32。
- 改动：设置“大模型”菜单里的列表/编辑/测试连接、默认主/决策模型、Key占位、Keep/Set/Clear、loading、Conflict 提示；复用组件。
- 验收：E01；重启状态正确；不回显已存 Key；截图和测试日志无 Key；Esc/关闭/主题/键盘可达。

### T014 Agent直接配置/内置能力/预算 UI
- [ ] 依赖：T011–T013；设计 §31–32。
- 改动：设置“工具与MCP”菜单含工具/本地Server/内置插件/审计tabs；Agent主页面=左侧列表+右侧三分区（基本/模型与提示词/工具与内置能力）+底部保存栏（非弹框）；无权限/运行限制/目标/路由/费用字段；提示词直接输入，无Jev专用配置/版本下拉；失败保留草稿。
- 验收：界面修改实际对应独立表；CAS 冲突可见；无整体配置覆盖；加载损坏实体不自动重写空值。

## M2：主 Agent 和原生工具闭环（P0）

### T015 会话/轮次/消息/event 持久化
- [ ] 依赖：T006、T012；设计 §9–12、§42.11。
- 改动：message header+parts、公开reasoning/plan/ref、UI state及panel preferences（mode/两宽/last session/新草稿）、稳定ID/序号/局部checkpoint、配置快照/活动Turn约束；正文单一真相不重复content_json。
- 验收：并发seq/part ordinal唯一、FK归属、250–500ms合并partial、迟到事件不污染新Turn；关闭重开/强杀恢复公开摘要/工具/审计/展开/滚动锚点；不持久化私有CoT，不每token写一行。

### T016 后台 actor、控制队列与 UI 事件桥
- [ ] 依赖：T003、T015；设计 §12。
- 改动：内部 runtime 创建和生命周期；控制/事件 bounded channel，steering/cancel；不持 UI/SQLite 锁跨 await。
- 验收：主窗口可响应；取消事件不被大文本挤掉；关闭窗口任务安全结束；错误可见。

### T017 主模型适配与秘密 transport 边界
- [ ] 依赖：T003、T008、T016；设计 §8、§17。
- 改动：generation Provider 流、模型能力和工具名称映射、请求前 credential lease、usage。
- 验收：调试默认 scripted，真实模型调用要等 T030 预算就绪；模型请求/日志/快照无 Key；HTTP 默认跨域 redirect 不带认证；分片/断流/429/截断测试；版本与 capabilities 匹配。

### T018 主循环与工具调用状态机
- [ ] 依赖：T017；设计 §10–11。
- 改动：prepare→stream→校验工具→执行→观察→下一轮；安全边界吸收用户补充。
- 验收：真实工具错误进入下一轮；输出截断不执行；同参数无进展可停止；不是固定意图路由；无 Jev 完整运行。

### T019 工具 registry、schema、发现与结果协议
- [ ] 依赖：T018；设计 §20。
- 改动：ToolDescriptor/Receipt、registry search/describe、Provider名称映射、结果分页/截断。
- 验收：未知工具/错误 schema 返回受控错误；发现不授权；input/output 大小限制；receipt 与模型/UI projection 分离。

### T020 目标授权、风险策略与审批表+询问AI内联批准卡片
- [ ] 依赖：T019；设计 §25、§29、§42.11。
- 改动：内核权限交集、最终plan hash、强制SecurityAuditHook与append-only审计表、scope/version、审批暂停/决定；denied 与 unknown 不误作可重试。
- 验收：参数/目标/plugin digest 改动审批失效；撤销即时生效；deny优先；批准不绕过数据库权限；卡片状态回显与刷新后仍可处理；无全局永久允许开关。

### T021 自动捕获当前界面并固定执行 scope（无用户确认）
- [ ] 依赖：T016、T020；设计 §13。
- 改动：UI capture 小型 DTO，Turn target pin、显式 retarget/缓存失效；不打包连接敏感配置。
- 验收：E04；切标签不改 SQL/Redis 目标；retarget 使旧批准失效；无目标时解释任务仍可进行。

### T022 `@` 引用、候选检索与附件 UI
- [ ] 依赖：T014、T021；设计 §14、§31。
- 改动：ContextRef/chip、多对象候选、失效/同名处理、文件选择与授权。
- 验收：E09；只传显式引用/摘要，详情按需；@ 不给写权限；键盘选择、Esc、外部点击正确。

### T023 元数据探索和显式 SQL 执行服务
- [ ] 依赖：T019–T021；设计 §4、§20–21。
- 改动：复用 Connector；从既有 scope query 方法提取独立服务，明确 QuerySessionId、history 与 source；不构造 GUI tab 执行后台任务。
- 验收：搜索/描述/查询真实测试库；取消/进度正确；Agent 与编辑器临时表/事务不串；既有查询回归通过。

### T024 SQL 只读保证、风险分类和修改执行
- [ ] 依赖：T023；设计 §21。
- 改动：对应 dialect AST、服务端读取策略、db.query/db.execute/db.explain、批量计划/审批。
- 验收：WITH 修改、SELECT INTO、副作用函数、EXPLAIN ANALYZE、锁语句、未知语法、跨 schema；E05；不能保证的数据库明确降为需授权风险，不宣称安全读取。

### T025 Redis 探索与命令执行工具
- [ ] 依赖：T019–T021；设计 §22。
- 改动：SCAN/inspect 范围工具、命令/子命令/参数风险、Cluster/DB scope、长订阅停止。
- 验收：E08；大集合不无限加载；COUNT 非硬上限测试；Lua/FLUSH/ACL/CONFIG 风险；Cluster 作用域和取消反馈真实。

### T026 文件搜索/读取与隐私边界
- [ ] 依赖：T019–T022；设计 §14、§29。
- 改动：Path-based grant、canonical/symlink 安全打开、文本/CSV 分段、二进制反馈；禁止凭据文件读取。
- 验收：E09；Windows 路径/UNC 等按授权策略测试、Linux/macOS symlink/编码；不能绕过目录授权读取 config.toml/sqlite 凭据。

### T027 结果句柄、分页与保留管理
- [ ] 依赖：T023–T026；设计 §15、§42.7。
- 改动：ResultRef、memory/retained/expired、spill 配额、source 归属，result.read_page。
- 验收：大结果不全进上下文；过期/重启内存结果有明确错误；retained 的路径/hash/清理可验证。

### T028 AI 对话面板/时间线/审批/产物容器
- [ ] 依赖：T018、T020、T022、T027；设计 §31。
- 改动：原生 composer、流式文字/工具状态、发送/停止、等待审批、loading 和结果引用。
- 验收：E02–E05、E09、E13；真实窗口启动；普通控件复用组件、颜色主题、图标、弹层防穿透；无需 Jev。

### T029 停止、重启和未知副作用恢复
- [ ] 依赖：T015–T028；设计 §11、§21。
- 改动：启动扫描 interrupted/running calls；重新核实、重新审批、禁止未知写重放；关闭窗口取消。
- 验收：E11；故障注入 prepared→执行→回执各窗口；读取消和不可取消写分清；副作用调用计数证明没有重复执行。

### T030 主循环预算预留与计量
- [ ] 依赖：T010、T017；设计 §30、§42.8。
- 改动：SQLite短事务预留/结算/unknown、并发/时间/iteration限额、价格快照；金额上限可选默认NULL，严格费用模式才要求已知价格，执行保护用默认值。
- 验收：E12；并发不能重复花同一余额；同请求结算幂等；unknown usage 不当免费；无足够价格的严格预算拒绝。

### T031 无 Jev 核心 E2E 门槛
- [ ] 依赖：T005、T028–T030、T065；设计§37–39。
- 改动：串行 E01–E05/E08/E09/E11/E13 自动化脚本与后端断言。
- 验收：数据库/文件实际结果、UI 小图、每步 checkpoint、decision 请求数=0；不能只检查聊天文案。

## M3：可选决策、上下文、Hook 与 BI（P1，安全边界仍 P0）

### T032 注册插件能力与确定性 Hook pipeline
- [ ] 依赖：T012、T019、T020；设计 §23–25。
- 改动：BuiltinRegistry、12类内置能力、observer/interceptor、priority/conflict、失败策略/deadline；真实receipt不可改，必经审计不能禁用。
- 验收：E12；before-tool 修改后重新校验审批；after-tool 不伪造成功；递归/timeout 受限；禁用后无调用。

### T033 基础模型路由与配置 UI
- [ ] 依赖：T030、T032；设计 §17。
- 改动：固定用户模型/候选过滤/默认规则、能力和隐私/价格判断；模型选择事件。
- 验收：模型角色绑定具有 FK/CAS；固定模型不被静默覆盖；经济/高能力标签不硬编码；无路由插件照常运行；换模型后工具配对和上下文兼容。

### T034 自动界面捕获、截断续读与上下文装配
- [ ] 依赖：T021、T027、T032；设计 §13、§15–16。
- 改动：按最终模型预算、保守 token estimate、完整工具配对、ResultRef/媒体引用和保护条目。
- 验收：用户约束/目标/口径不丢；不无限加载全库/文件；切模型重新估算；图片限制 decoded 像素/活动数量。

### T035 摘要模型与原子 checkpoint
- [ ] 依赖：T034、T030；设计 §16、§42.11。
- 改动：独立摘要模型配置/默认主模型、生成/保护校验/提交、失败保留旧 checkpoint。
- 验收：E07；截断/出错摘要不覆盖；失败后卸载或可见暂停；压缩费用入 ledger；原始记录可查。

### T036 System One/Jev decision Provider
- [ ] 依赖：T017、T030、T032；设计 §18；参考本地 Pi System One 两个 adapter。
- 改动：Choice/Score 协议和概率/数值校验、deadline、取消、隐私、usage；不走 generation.tool_call。
- 验收：mock/可选真实服务验证；不猜 provider 返回 schema；非法数值/置信度拒绝；密钥不出宿主。

### T037 Jev 路由/相关性插件与故障策略
- [ ] 依赖：T033–T036；设计 §18。
- 改动：内置Jev自动reconcile：唯一合格模型/明确默认即生效，system binding不覆盖user；多候选待选；按需决策/过滤/熔断，开关UI、禁止required。
- 验收：E06；未配置/禁用零Jev请求；仅保存启用合格模型即可自动使用，无需安装/手工实例；超时/无 Key/invalid/低置信度回基础路径；不擅自选贵模型；不每轮等待/弹错。

### T038 业务知识与按需 Skills
- [ ] 依赖：T006、T022、T034；设计 §19、§42.10。
- 改动：独立 knowledge 表 CRUD/CAS、scope、来源/确认、插件 skills 注册/检索。
- 验收：跨库口径不混用；模型提议不自动成为事实；用户确认/退休立即影响新任务；大知识不全量加载。

### T039 产物 schema、source 引用与内置 BI 插件
- [ ] 依赖：T027、T032、T038；设计 §27、§42.7。
- 改动：报告/图表/表格声明、创建/增加/完成工具、source table、口径/时区/截断校验。
- 验收：数字来自实际结果，不由模型编造；来源同任务且可解析；renderer 失败不改 SQL 结果；插件可替换。

### T040 原生图表/报告 UI 与安全导出
- [ ] 依赖：T028、T039；设计 §27、§31。
- 改动：可信宿主renderer、会话产物卡片→独立报告弹框（非设置页）、来源/SQL/导出、主题/可访问性、Markdown/受控HTML、路径授权与 staged→ready；后续 PDF/XLSX 单独评估。
- 验收：E10/E13；脚本/远程资源禁止；文件实际内容/hash；卸载 renderer 仍可解释元数据；导出失败无假下载链接。

### T041 M3 串行界面和真实模型评测
- [ ] 依赖：T031、T032–T040；设计 §37–39。
- 改动：E06/E07/E10/E12 脚本；同模型同 fixture 的真实自主任务集。
- 验收：无 Jev 全套仍通过；scripted 稳定性与真实智能分开报告；真实质量/费用/无依据结论有证据；每步小图/checkpoint。

## 扩展接口与未来事项（不作为本期交付依赖）

### T042 第三方扩展接口预留（本期，不实现加载）
- [ ] 依赖：T032；主设计§26、主设计§26。
- 改动：host_api_version/RegistrationSource/能力trait/manifest schema保留；实际只BuiltinRegistry，不引入Wasm/JIT/JS/dylib依赖。
- 验收：外部来源返回Unsupported；设置页无假安装按钮；将来能通过同一ScopedHost/权限/审计扩展；不需要沙箱原型/包体测试。

### T043 插件安装/升级/禁用/卸载【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T012、T042；设计 §24–26。
- 改动：staging/digest/manifest/授权、原子包目录切换、数据库 revision 和重启恢复；UI 状态。
- 验收：E14；zip-slip/压缩炸弹/错误 ABI/未知包不执行；活动 Turn 版本固定；失败不删除旧包/实例。

### T044 插件/MCP integration secrets 与宿主代理【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T008、T043；设计 §24、§42.10。
- 改动：独立 integration secrets AES 行、引用/owner CAS、权限受控宿主出站；模型秘密仍在模型行。
- 验收：插件拿不到 raw Key；配置 JSON 无凭据；错钥匙/失败保存/owner 删除正确；系统密钥库调用=0。

### T045 可选 MCP Client stdio/HTTP 工具桥【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T019、T020、T044；设计 §28。
- 改动：作为 Client 接入外部 Server，发现/过滤/命名空间/schema、deadline/取消、stdio 环境隔离、HTTP redirect/身份；不是用户要求的对外 Server。
- 验收：未启用不启动进程；只读标注不替代风险检查；服务器挂掉不影响原生工具；E14 扩展步骤。

### T046 MongoDB 工具包【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T019–T020、T043；设计 §28。
- 改动：collection/schema sampling、find/aggregation/write，复用 Connector 扩展或职责实现；按能力选内置/可安装，不硬塞 SQL 接口。
- 验收：`$out/$merge`、脚本、批量/取消/unknown、取样截断；当前阶段未实现要明确，不伪装已有支持。

### T047 HTTP API 工具包【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T020、T044；设计 §28。
- 改动：域名/IP/redirect/SSRF、auth refs、method/body schema、响应截断/产物。
- 验收：GET 也不自动认为无副作用；loopback/内网策略显式；秘密不进入日志、上下文和报告；无授权无请求。

### T048 SSH 工具包【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T020、T044；设计 §28。
- 改动：host/user/auth ref、known host、命令与远程文件空间、流式输出/取消。
- 验收：未知/变化主机 Key 不自动接受；Windows/Linux/macOS 分开验证；会话关闭和未知副作用正确。

## M5：发布、数据生命周期与回归（P0）

### T049 安全审计与泄漏测试
- [ ] 依赖：M1–M3；未来扩展接入后重跑；设计 §29、§33–34。
- 改动：known secret 扫描 public DTO、Debug、错误、日志、事件、导出、snapshot、图片、插件 payload；不要打印匹配到的 secret。
- 验收：Set/Keep/Clear 的所有异常路径零泄漏；prompt injection 不能扩权；Hook/MCP 无绕过内核的执行入口。

### T050 保留/清理/安全导出与测试快照
- [ ] 依赖：T015、T027、T039；设计 §35–36、§42.12。
- 改动：归档分页/依赖清理、spill/产物空间、报告安全导出；一致快照只作测试helper，不做完整备份UI/配置导入产品。
- 验收：活跃任务不被清理；FK 始终开启；测试一致快照实际可读且保持原连接/AES，无密钥/密文进入证据；文件/表 staged 恢复不丢已完成文件。

### T051 明确的共享密钥轮换（非自动迁移）【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T007–T009、T050；设计 §34。
- 改动：显式 UI/确认、双阶段配置/数据库 journal、所有连接+模型+integration 密文认证和重加密。
- 验收：每个 crash point 可恢复；错误密钥不覆盖旧密文；connection_secret_key→secret_key 不变更实际 Key 的兼容测试；绝不只改 TOML 就宣布轮换完成。
- 发布说明：首版可延期此功能，但必须明确不支持手工改 Key；新安装/旧安装密钥策略不能延期。

### T052 三端性能、界面与真实任务总验收
- [ ] 依赖：T041、T049–T050、T064–T066；未来功能不计本期验收。
- 改动：Windows/Linux/macOS 启动与 E01–E14 按功能启用范围；长结果/会话/插件内存；真实模型预算受限评测。
- 验收：CPU/内存/任务取消/磁盘与截图预算记录；核心无 Jev 成功；可选系统失败不挂主窗口；三端截图/断言/检查点，未测范围不标通过。

### T053 用户说明和最终交付核对
- [ ] 依赖：T052。
- 改动：用户指南模型/Key/Agent/权限/@/插件/报表/停止/恢复；同步设计与真实 schema/版本。
- 验收：已实现/延期清晰、依赖版本固定、参考 commit/许可证记录、任务证据索引；文档不再描述不存在的已实现接口。

## M2-MCP：对外本地 Server 与具体数据库工具（P0/P1，独立于 Jev）

本阶段可在 M2 工具基础上交付，不依赖Jev或未来第三方加载，不要求用户配置任何主模型/Jev/Agent prompt。下列为新增 T054–T063，这是初次MCP增补；加设置/审计/Docker专项后共66项；首版可只完成只读/明确 scoped write，T063 桌面桥是单独扩展。

### T054 固定 MySQL/Redis 工具契约与公共执行服务
- [ ] 依赖：T002、T019、T020；主设计§20.4–20.6、§21–22、§28.4–28.5。
- 改动：关系型5/Redis4/公共4工具、External MCP Caller/Owner、schema/output/dynamic registry、target资源句柄；MySQL/Pg/SQLite 不复制三组工具。
- 验收：对外与内部调用同一执行服务、同一 receipt/权限；无外部 Agent 或主模型依赖；不能把 tools/list 可见性当授权。

### T055 MySQL/PostgreSQL 参数真绑定与受控查询/执行
- [ ] 依赖：T002、T023、T054；主设计§21.1–21.6。
- 改动：ParameterizedQueryRequest、typed SqlParameter、Connector 默认 Unsupported 的新参数能力，MySQL SQLx/PG对应驱动bind与dialect占位符、受控fetch而非fetch_all后分页；精确decimal/大整数协议、巨大cell解码/预览边界。
- 验收：参数计数/NULL/Unicode/SQL注入/decimal/u64/binary/超限；不得字符串拼值或忽略参数；跨库/副作用/多语句/DDL风险、真实只读角色/事务、EXPLAIN非ANALYZE；旧 QueryRequest 路径回归。

### T056 Redis AI/MCP 解码预算与类型化 inspect
- [ ] 依赖：T025、T054；主设计§22.1–22.5。
- 改动：RedisReadBudget 到 RESP parser、bulk/total/elements/depth、小预算专用路径和超限连接丢弃；按TYPE读取/Key命令规则。
- 验收：合法巨型bulk/百万元素在大分配前拒绝；HSCAN COUNT并非硬限额；未消费帧不回池；保留桌面已有下载路径；TTL-1/-2、binary、Cluster scope、AUTH/SELECT/EVAL/FLUSH 等旁路被拒绝。

### T057 Server Profile/资源授权/session/request 独立表
- [ ] 依赖：T006、T054；主设计§28.6、§42。
- 改动：新增4表、逐对象CAS、MCP独立审计；result/approval owner扩展，不能建假Turn；连接引用列不破坏现有connections全量保存。
- 验收：默认disabled/空授权/readonly；MCP Client/Server配置不混用；跨profile/session FK拒绝；revision改变旧句柄失效；protocol request ID可后续复用，不当永远唯一幂等键。

### T058 Headless fluxdb-mcp stdio 二进制
- [ ] 依赖：T054–T057；主设计§28.5、§28.8。
- 改动：workspace新member、官方rmcp选定版本、initialize/tools/list/call、typed schemas、stdout协议/stderr日志、bounded transport、进度/取消/EOF。
- 验收：没有GUI/fluxDB模型APIKey/Jev/Agent模型配置也能运行；所有数据操作走app服务；无秘密CLI/env；frame/deadline/并发/内存受控；CLI消费者能解析最终结果与isError。

### T059 MCP资源过滤、受限写与操作状态
- [ ] 依赖：T020、T058；主设计§20.5–20.6、§28.7–28.8。
- 改动：MySQL-only/Redis-only/readonly工具集、fresh grant检查、query/execute分离、profile scoped write、operation status/cancel、owner result_page。
- 验收：隐藏写工具仍直接调用拒绝；Claude allowTools不绕過宿主；另一库/Keyprefix/DB禁止；MCP未知写不重试；只查已有snapshot的分页不重复SQL；无审批桥时明确Unsupported/ApprovalRequired。

### T060 Server管理页、配置导出和活动审计 UI
- [ ] 依赖：T005、T057–T059；主设计§39.1。
- 改动：本地Server Profile/授权资源页面与不含秘密的Claude MCP JSON导出，三端绝对binary路径；MCP活动页显示profile/session/request、状态/目标/摘要。
- 验收：主窗口开启/禁用/收窄scope真正影响server调用；stdout与UI请求ID对齐；小图/检查点；不把MCP Client页面冒充Server设置。

### T061 纯协议 MCP 集成测试
- [ ] 依赖：T058–T060、T065；主设计§28.8、§37.2。
- 改动：复用已配置Docker MySQL/PostgreSQL/Redis、本run namespace fixtures、initialize/list/call/schema/structuredContent/isError/cancel/EOF、日志污染/帧超限/未知写崩溃窗口。
- 验收：真实后端数据/调用计数/FK审计断言，过程不使用付费模型；只读scope和默认关闭写入；Client信息不能提升权限；每阶段保存有界证据。

### T062 使用 Claude Code CLI 实测 MCP
- [ ] 依赖：T061；主设计§37.2、§39.1。
- 改动：按实际CLI help构建 --mcp-config/--strict-mcp-config/--tools ""/精确allowedTools/-p 有界运行；MySQL与PostgreSQL探索查询/Redis扫描查看/错误修正/拒绝与scoped写。
- 验收：确有MCP tool-use与server回执，不能凭模型文字或Connected判通过；真实后端断言+对应UI小截图；不改全局MCP、不skip权限、不碰生产库；费用/调用/时间总预算，auth不可用标blocked。

### T063 可选桌面人工审批桥【延期，不属本期】
- [ ] **延期：本期不实施、不阻塞发布。** 原依赖：T020、T058–T062；主设计§28.7。
- 改动：Windows named pipe/Unix domain socket经平台隔离，认证/ACL、planhash/CAS、桌面AppCommand批准/拒绝；进程断开使未执行批准失效。
- 验收：模型无法批准自己；无GUI不伪装等待UI成功；旧参数/旧grant审批失效；只有实际桥可用才启用desktop_approval；三端截图/后端证明不重复写。首版延期时必须明示不支持该模式。

## 设置/时间线/真实Docker测试专项（本期 P0/P1）

### T064 设置菜单和原生时间线视觉验收
- [ ] 依赖：T013/T014/T028/T037/T040/T060；主设计§31.1–31.6。
- 改动：SettingsPanelSection增加大模型/工具与MCP，枚举/标题/render齐全；默认抽屉/记忆上次形式、面板内切换、60%/40%硬上限/最小与拖拽、标题新建/关闭；公开思考/计划、折叠工具与审计、空/错/loading/unknown状态、主题和无障碍。
- 验收：U01–U03；原生组件/UiColors/AppIcon；询问AI字号缩小但保持可读、设置表单不全局缩小；不伪造不支持模型的CoT，分组执行时间线保留失败/纠正；BI报告弹框不放设置页，高级保护非首次前置；不每token创建卡片；U07/U08的宽度/模式/会话/新草稿/关闭不停止与不重放；上滚/折叠不被抢；每步小图与检查点。

### T065 必经安全审计Hook与持久化展示
- [ ] 依赖：T002/T020/T054/T057；主设计§25.1、§42.11。
- 改动：最终参数patch后执行强制BuiltinSecurityAuditHook、owner互斥audit表、规则/hash/version、执行前revision变动再审计；内外工具同链。
- 验收：审计无法执行/保存fail closed；不能禁用、不能被Jev/插件Allow覆盖；修改参数旧audit/approval失效；重启显示同一判定，无Key/秘密载荷。

### T066 当前已配置Docker连接与持久化E2E
- [ ] 依赖：T004/T005/T015/T029/T061/T062/T064/T065；主设计§35.1–35.2、§37.1、§39.2。
- 改动：读当前配置选择MySQL/PostgreSQL/Redis，一致配置/AES快照为test root，网络实际连原Docker；本run database/schema/keyprefix；正常退出/强杀/重开message parts/工具/审计/UI state；上次模式/两种宽/会话或新草稿、切换不重新执行。
- 验收：不用新手填连接/猜端口，不重复询问本run读写DDL；数据/SQL/target/audit/UI三方断言；只清本run对象、不FLUSHALL/重置容器；快照含Key文件不进证据；无实际运行时如实标未测。

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

文档阶段 D004 只运行文档/SQL静态与内存数据库检查，不消耗真实模型费用、不改真实用户数据库、不宣布三端 GUI 已验证。

## G. HTML交互稿评审范围

完整入口：[ai-agent-ui.html](/Users/shining3d/fusuwei/code/fluxDB/docs/prototype/ai-agent-ui.html)。包含所有当前AI Agent管理/运行页面，不只询问AI。用户参考图的分组时间线/报表卡片用于视觉方向；报告是产物弹框，费用/执行保护是高级配置。

浏览器测试使用模拟数据、步骤检查点和受限JPEG，不能据此勾选原生GPUI、Docker、模型或Claude MCP任务完成。真实Key不进入原型localStorage。

## H. Agent配置简化

T011/T014以主设计§19、§31.8为准：提示词直接在Agent输入，没有独立模板、多版本/发布，没有Agent内Jev配置。其余模型/工具/内置能力/权限/预算/运行参数必须对照字段矩阵实现，不能只复制参考稿外观。HTML交互稿同步取消提示词菜单，缩小询问AI字体。

## I. 智能Agent简化

按用户要求：Agent不要求用户选择/确认执行目标（自动捕获当前界面）、不做固定连接/库/DB/schema字段、不做选择策略（属路由插件）、无用户费用上限。主设计§13/§30.1/§31.8为唯一依据；DDL已删除4列。HTML交互稿同步删除这些字段。

## J. 审批卡片

T020按主设计§31.9实现询问AI内联批准卡片：参考Goose三态按钮与行内确认结构，去掉永久Always Allow，改为绑定plan hash的10分钟范围授权；Esc/遮罩=拒绝。权限与运行限制不是Agent用户配置（连接只读/内核审计/§30.1内部护栏）。
