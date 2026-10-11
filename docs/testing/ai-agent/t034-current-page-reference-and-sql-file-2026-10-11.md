# T034 补充：当前页面 AI 引用与打开本地 SQL 文件（2026-10-11，macOS arm64）

日期：2026-10-11。平台：macOS arm64。分支：`codex/current-page-and-sql-file`。

## 1. 目标

- 把“当前页面”当作一种自动、可查看范围、可关闭的特殊 `@` 引用，按内容类型选择全文、摘录或结构化摘要。
- 移除编辑器的 256 KiB 发送门槛：大 SQL 保留共享不可变快照，`editor.search`/`editor.read_range` 覆盖全文。
- 表数据、Redis Key 值与结构信息默认不自动发送；只有用户对当前页签明确开启“数据共享”才附带已加载快照。
- 查询工具栏增加纯图标“打开本地 SQL 文件”，默认新页签打开，不覆盖未保存内容、不自动执行。
- `AGENTS.md` 增加：新增页面或页面功能时必须说明 AI 可见范围，未接入时询问用户是否接入。

## 2. 实际执行的命令与结果

```bash
cargo fmt --all                 # 通过
cargo fmt --all -- --check      # FMT OK
cargo check --workspace --locked --offline   # 通过（仅既有 block v0.1.6 future-incompat 警告）
cargo test -p fluxdb-app --locked --offline  # 795 passed / 0 failed / 1 ignored
cargo test -p fluxdb-desktop --locked --offline current_page   # 5 passed
cargo test -p fluxdb-desktop --locked --offline sql_open_file   # 2 passed
cargo build -p fluxdb-desktop --locked --offline                # 通过
```

`cargo test -p fluxdb-desktop --locked` 全量：614 passed、1 failed。唯一失败
`ai_chat_model_selection_disabled_model_error_preserves_selection_and_input` 已用
`git stash` 在改动前基线上复现同样失败（断言期望“不可用”，实际文案为“已停用”），
属既有失败，非本次回归。

## 3. 覆盖场景

- 大 SQL（每段约 300 KiB、唯一关键词在中段）：摘录不含中段关键词、不写全文快照；
  引用 `delivery=excerpt`、`capabilities` 含 `editor.search`/`editor.read_range`；
  `editor.search` 能命中中段关键词。
- 表数据页：默认只发列名/类型/主键、筛选排序、分页与状态；数据值不进入模型上下文；
  开启“数据共享”后仅对当前页签生效，生成 `page.read_rows` 能力但不把值塞进首条消息。
- 页面开关：关闭当前页面上下文后不再附带；数据共享开启后再切到别的页签不会自动继承。
- 打开本地 SQL：`.sql` 内容（含 BOM）进入新页签并记录本地保存目标；原页签未保存内容保留；
  取消或选择非 `.sql` 文件时原内容不变、不新建页签并给出提示。
- 配置库隔离：页面数据来源连接到应用自身 SQLite 配置库时被拒绝，与显式引用一致。
- `AGENTS.md`：新增页面上下文说明与询问要求。

## 4. 真实桌面验证

隔离 fixture：`ai_panel_fixture` 生成 `/private/tmp/fluxdb-current-page-fixture-20261011/config`；
以 `FLUXDB_AI_TEST_ROOT` + `FLUXDB_PROFILE=currentpage-verify2` 启动真实窗口，
AI 面板底部“当前页面 · …”控件（含可见范围说明、开关、数据共享开关）已渲染可见。
截图：`/private/tmp/fluxdb-composer-zoom2.png`。

## 5. 未验证 / 边界

- 仅验证 macOS；Windows / Linux 未验证。
- 当前页面覆盖 SQL 编辑器、表数据/结构、Redis Key/数据页与 Redis 命令工作台；
  Redis CLI、Pub/Sub、表设计、ER、用户管理与设置页仍未接入正文（只给目标定位），
  按 `AGENTS.md` 在后续页面开发时询问用户是否接入。
- 保存查询/选区/结果集显式引用仍是既有有界快照，未在本轮改造为完整共享。
- 打开 SQL 文件只支持 UTF-8 `.sql` 文本，不支持编码自动识别。
