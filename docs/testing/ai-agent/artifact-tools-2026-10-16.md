# 产物工具与内置 BI 能力解耦

实际平台：macOS arm64。

## 行为

- `artifact.create`、`artifact.add_block`、`artifact.finalize` 独立注册，出现在工具目录及 Agent 工具配置中；全局工具开关、Agent 白名单、审计与权限链保持生效，不新增对外 MCP 工具。
- BI 全局实例或 Agent 绑定关闭时，下一轮工具描述不声明 BI renderer；实际产物操作返回 `artifact_capability_disabled`，而不是 `unknown_tool`。
- 启停状态在每次产物操作开始时重新读取；不强制中断已进入执行中的操作。追加和完成被拒绝时保留原草稿及 revision，不误标为生成失败。
- 重新启用能力后可继续使用工具；完成报告的查看和静态导出不依赖插件启用状态。
- 内置 BI 仍只接受本轮 SQL 查询来源，不把 Redis 输出作为表格来源。未新增第三方代码加载或自定义模板功能。

## 验证

```sh
cargo fmt --all
cargo check --locked
cargo test -p fluxdb-app --locked --lib
cargo test -p fluxdb-app --locked --test ai_bi_artifacts --test ai_artifact_report --test ai_conversation_tools_guard
cargo test -p fluxdb-desktop --locked ai_ui_artifact_tools_remain_configurable_when_bi_plugin_is_disabled
cargo test -p fluxdb-desktop --locked ai_ui_agent_tools -- --test-threads=1
cargo test -p fluxdb-desktop --locked ai_tools_catalog_tests
cargo test -p fluxdb-desktop --locked ai_ui_builtin_plugins_page_lists_catalog_and_enforces_mandatory
cargo test -p fluxdb-desktop --locked ai_artifact_card_ -- --test-threads=1
git diff --check
```

- 格式与编译检查通过；编译仍有既有未使用函数、依赖未来兼容性及本地 C 库 deployment target 警告。
- App 单测：886 通过，10 忽略。沙箱内先有 15 项因无法绑定本地 Mock 端口失败；获准在沙箱外重跑全部通过。
- App 集成：产物 27、报告读取与导出 11、会话工具保护 6，共 44 项通过。覆盖全局禁用、Agent 禁用、创建后禁用、重新启用、草稿不变、来源结果不变及历史报告继续查看和导出。
- GPUI Headless：6 项通过，覆盖插件关闭后工具仍显示、全局工具开关独立落库、Agent 权限保存、插件开关，以及真实报告弹框与导出；另有目录单测 3 项通过。
- 新增工具行后，既有 MCP 状态工具位于首屏之外；对应点击测试改为先真实滚动，再点击与断言，未绕过交互链路。

所有数据库用例使用隔离临时 SQLite 数据与受控 Provider，不访问生产数据库或真实模型。

## 真实桌面限制

已执行以下命令并确认桌面进程启动日志，使用独立 root/profile；验证结束后已退出该测试进程：

```sh
FLUXDB_AI_TEST_ROOT=/private/tmp/fluxdb-bi-capability-ui \
FLUXDB_PROFILE=codex-bi-capability \
FLUXDB_DEBUG_AI_SETTINGS=tools \
FLUXDB_DEBUG_AI_TAB=plugins \
cargo run -p fluxdb-desktop
```

桌面控制服务两次返回 `Sky Computer Use service startup request failed`，无法读取或操作可见窗口。本轮没有完成真实桌面点击、截图及明暗主题视觉验收，不将进程启动或 Headless 结果视为桌面 E2E 通过。Windows/Linux 未编译与运行。

设置页当前 AI 上下文维持原有边界：仅页面类型及定位，不读取工具目录、插件状态或设置表单正文；本次未扩大读取或上传范围。
