# 国际化 E2E 测试规范

适用于语言包发现、语言切换和组件翻译接线的跨层验证。通用验证范围仍以根目录
[AGENTS.md](../../AGENTS.md) 的“测试与验收”为准。

## 测试目标

验证用户不需要改代码即可增加语言，并验证一次切换会同时更新以下可观察结果：

- 设置页能发现并显示外部语言包。
- 真实 `NavicatMain` 控件能触发语言切换。
- 窗口标题和 GPUI Component 内置文案使用新语言。
- 语言选择写入设置，重新解析后仍选择该语言。

## 测试层级

`apps/fluxdb-desktop/src/main_parts/i18n_tests.rs` 中的
`external_language_pack_is_discovered_and_applied_from_settings` 是无头 E2E：

- 使用临时 profile，不读写用户 dev 数据。
- 使用生产 `navicat_main_new` 和 `Root` 装配。
- 首页设置入口、语言下拉和下拉选项通过 `VisualTestContext::simulate_click` 驱动。
- 语言保存按钮通过真实控件事件触发。

测试前置会把设置页当前分区置为外观页，避免把侧边栏导航当成语言功能断言；
分区切换本身不属于本用例的验证范围。被断言的语言切换、保存和文案更新都不能
直接调用处理函数。

语言包解析、回退、覆盖和错误隔离由 `crates/fluxdb-i18n/src/lib.rs` 的单元测试
覆盖。它们不能替代 E2E，因为单元测试不能证明 GPUI View、设置保存和组件扩展已经
正确接线。

同一测试模块还有 `every_literal_ui_key_exists_in_bundled_packs`：扫描桌面源码中的
字面量翻译 key，检查 `en` 与 `zh-CN` 语言包没有遗漏。它不能证明译文正确，但能
防止新增界面文案只改代码、忘加语言包。

`bundled_packs_have_identical_keys_and_placeholders` 校验内置 `en` / `zh-CN`
语言包 key 集合完全一致，且每条译文的 `%{...}` 占位符集合一致。
`dynamic_translation_tables_reference_existing_keys` 扫描后端错误映射和 AI 护栏
映射中的动态 key，确认两套语言包都存在对应词条。

`backend_error_messages_translate_known_text_and_keep_unknown` 覆盖静态错误、动态
错误模板和未知错误原样保留；`ai_guardrails_translate_known_text_and_keep_unknown`
覆盖 AI 审批护栏的展示翻译与未知护栏回退。
`builtin_plugin_text_translates_known_declarations_and_keeps_unknown` 覆盖内置插件
名称、配置字段、Jev 状态与插件配置错误的展示翻译，并确认外置插件原文不被改写。
`builtin_tab_titles_follow_locale_without_changing_state` 覆盖设置、AI、Redis CLI、
备份和 ER 标签页标题的展示翻译，确认只改显示文本、不改底层 `TabState.title`。

`restore_action_plan_keeps_stable_values` 是局部单测：确认恢复弹框的逐表动作候选
保存的是 `RestoreTableAction` 稳定值，默认项是显式占位而不是任何翻译后的标签。

`pg_draft_option_labels_match_under_switched_locale` 覆盖一个真实踩过的坑：PG 常规页的
「密码操作」「密码有效期」下拉把选中值标签回灌业务枚举，因此展示与匹配必须来自同
一份翻译。用例在切换到外置语言后断言标签随之变化。

## 建表下拉接线场景

`create_table_inheritance_uses_stable_values_from_localized_dropdown` 是第二条无头 E2E，
验证外部翻译不会变成实际的 SQL 选项：

1. 临时 profile 的 `ja` 包将“继承数据库默认”故意翻译成 `InnoDB`，与真实引擎同名。
2. 用生产 Controller 命令准备未提交的 MySQL 建表草稿，再以 `navicat_main_new` /
   `Root` 装配生产 View。准备草稿不属于被断言的选项交互。
3. 真实鼠标事件点击“选项”页签和引擎 Select，依次选 `MyISAM`、`InnoDB`、继承默认。
4. 同时检查渲染出的已选标题与 `CreateTableState.engine`：
   前两次保存真实引擎名，继承项显示 `InnoDB` 但业务值必须是空串。
5. 外置包另提供“名称”的日文翻译 `名前`；真实点击索引、外键、检查与触发器页签，
   确认日文表头和缺失 key 的英文回退同时显示。
6. 点击各页真实的新增按钮，确认草稿新增对应条目，索引/外键的空 SQL 选项仍为空，
   检查默认不开启“Not enforced”，触发器时机/事件仍为 `BEFORE` / `INSERT`。
7. 点击分区页，确认表单标签按当前 locale 回退显示。

本场景不调用下拉处理函数、不手工发出 SelectEvent，不执行建表或连接数据库；
连接地址使用 `.invalid` 域名与端口 1，不预选数据库以避免外键页惰性加载元数据；
存储完全位于临时目录。
它不证明实际建表 SQL 的数据库执行结果，也不覆盖原生文件选择器。

`create_table_options_keep_sql_values_and_blank_option_meanings` 是局部单测，
补充确认索引/外键等非继承空选项仍为空白，不能误显示成“继承数据库默认”。
同模块的 `ai_scope_permission_keeps_stable_tiers_under_switched_locale` 也是单测，
不是 E2E。模块名包含 `e2e` 不代表其中每个用例都是端到端测试。

## Redis Stream 场景

`redis_stream_localized_validation_prevents_writes` 是 Redis 详情页的无头 E2E：

1. 临时 profile 写入仅覆盖 MAXLEN 错误文案的外置 `ja` 包，未覆盖的文案回退英文。
2. 通过生产 Controller 命令准备 Redis Key 数据页与已加载快照；准备数据不属于
   被断言的交互。
3. 真实点击 Stream 工具栏“新增”，在 Entry ID / Field / MAXLEN 输入框中键入内容，
   点击保存。
4. 断言底部提示显示外部包日文、抽屉保留草稿、输入值未被改写，且写任务表为空。
5. 取消抽屉后，在时间范围输入非法值并点击应用，断言提示为英文回退、生效范围未变、
   未发起查询，已加载数据页和 Stream 条目缓存保持不变。

该用例还会回归表单布局：字段输入必须能通过真实点击获得焦点（此前的抽屉高度约束
会让字段区被固定按钮区覆盖）。它不连接 Redis、不执行写命令，也不覆盖真实服务端
返回或滚动条拖拽。

`redis_stream_validation_preserves_bounds_and_payloads` 是局部单测，覆盖 MAXLEN
上下界、时间格式、Entry ID 格式以及“只裁剪字段名、不改写字段值空白”的契约。

`localized_select_options_keep_stable_business_values` 是局部单测：切换中英文后
断言 SSH 认证方式、云提供方、外键检查、List 删除位置四组下拉的业务值不变，
只有标题变化。它们分别对应连接弹框、危险表操作和 List 删除抽屉的稳定值契约。

Hash/Set/ZSet/List 详情页复用同一套 `app.redis.*` 控件文案和本地化校验文本；
本批通过 `every_literal_ui_key_exists_in_bundled_packs` 与现有 Redis E2E 回归，
没有新增独立交互用例：它们没有引入新的状态机或写入路径。Hash 的字段级 TTL
校验仍保留“大于 0 或留空”的原规则，只替换错误文案来源。

## 场景步骤

1. 在临时 profile 写入合法的外部 `ja` 包，包含界面文案和一条
   `gpui_component` 覆盖。
2. 以 `en` 启动国际化初始化，确认语言列表出现 `ja` 且加载无错误。
3. 创建真实生产 `NavicatMain` 窗口并绘制首帧。
4. 点击首页“设置”。
5. 打开语言下拉并选择 `ja`。
6. 核对当前 locale、窗口标题和 GPUI Component 翻译。
7. 点击外观页“保存”。
8. 从存储重新读取设置，并重新初始化 i18n，确认仍选择 `ja`。

## 断言

必须同时断言观察结果和业务结果：

- `ja` 包在启动初始化返回的语言列表中存在。
- 切换后 `fluxdb_i18n::active_locale()` 为 `ja`。
- 窗口标题变为外部包提供的日文标题，这是真实 View 的可观察反馈。
- GPUI Component 的 `DatePicker.placeholder` 经 `extend!` 返回日文覆盖值。
- 连接表单的 Password/Database placeholder 在切换后刷新为语言包回退值，避免
  长生命周期 `InputState` 继续显示旧语言。
- 保存后 `Settings.locale` 为 `ja`。
- 使用保存值重新初始化后仍为 `ja`。

只检查语言文件能解析、只检查设置模型、只截图或直接调用切换处理函数，都不算
本场景通过。

## 执行

```sh
cd /Users/shining3d/fusuwei/code/fluxDB-i18n
FLUXDB_PROFILE=dev cargo test --locked -p fluxdb-i18n
FLUXDB_PROFILE=dev cargo test --locked -p fluxdb-desktop --bin fluxdb-desktop \
  i18n_e2e_tests -- --nocapture --test-threads=1
```

执行后记录实际通过数量和平台。测试不得启动可见应用、抢焦点或操作用户桌面。

## 未覆盖

- 原生 macOS 菜单、Windows/macOS 托盘菜单的语言切换仍需在对应平台实例中验证。
- 长译文、窄窗口和 CJK 字体回退需要无头渲染核对。
- RTL 镜像、复数规则和本地化日期/数字格式不在本轮能力内。
