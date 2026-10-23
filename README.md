# FluxDB

**原生跨平台数据库客户端与 SQL 编辑器：在一个窗口中完成查询、数据编辑、结构管理和 Redis 调试。**

FluxDB 使用 Rust + GPUI 构建，不依赖 Electron 或 WebView。它把 MySQL、PostgreSQL、SQLite、TiDB、达梦 DM8 和 Redis 放进同一套桌面工作流，并提供有明确权限边界的 AI Agent。

**简体中文 | [English](./README.en.md)**

[![Release](https://img.shields.io/github/v/release/fluxdb-alt/fluxDB)](https://github.com/fluxdb-alt/fluxDB/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/fluxdb-alt/fluxDB/ci.yml)](https://github.com/fluxdb-alt/fluxDB/actions/workflows/ci.yml)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue)](./LICENSE)
![Platforms](https://img.shields.io/badge/平台-macOS%20%7C%20Windows%20%7C%20Linux-blue)

[下载最新版本](https://github.com/fluxdb-alt/fluxDB/releases/latest) · [最近更新](#最近更新) · [功能一览](#核心能力) · [数据库支持](#数据库支持) · [操作手册](./docs/user-guide.md) · [反馈问题](https://github.com/fluxdb-alt/fluxDB/issues)

![FluxDB SQL 编辑器、表结构提示与补全](docs/screenshots/sql_editor_adapter.png)

## 最近更新

### v0.1.10

- **新增达梦 DM8 数据源**：连接管理、Schema 与对象浏览、SQL 查询、表结构设计与数据编辑。
- **新增表数据导入向导**：支持 TXT、CSV、JSON、XML，提供字段映射、追加 / 截断模式和导入日志。
- **AI Agent 增加页面与编辑器授权**：可按授权查找并查询表，也可改写指定 SQL 范围，但不会自动执行。
- **新增应用内更新**：从 GitHub Release 检查更新，并校验签名清单与安装包摘要。

[完整更新说明](./release-notes/v0.1.10/zh-CN.md) · [历史版本](https://github.com/fluxdb-alt/fluxDB/releases)

## 为什么选择 FluxDB

- **一个工作台，多种数据源**：在同一个窗口中管理关系型数据库和 Redis，减少工具切换。
- **从查询到写入的完整路径**：SQL 编辑、结果编辑回写、表结构设计、数据导入导出、备份恢复和 ER 图互相衔接。
- **原生桌面体验**：基于 Rust + GPUI，不依赖 Electron 或 WebView。
- **AI 有明确安全边界**：AI 只能读取有界数据、修改授权编辑器或执行受控操作；写入类工具需要审批。

## 核心能力

### SQL 与数据编辑

- SQL 编辑器：语法高亮、代码折叠、关键字 / 表 / 列 / 函数补全、SQL 格式化和查询历史。
- 数据浏览：分页、筛选、排序、行详情和单元格详情。
- 数据写入：编辑结果集并回写数据库，保留危险操作提示。
- 数据导入：支持 TXT、CSV、JSON、XML。
- 数据导出：支持 SQL、TXT、CSV、JSON、XML。
- SQL 文件执行：可执行已有 `.sql` 文件。

### 表结构与 ER 图

- 查看列、索引、外键、触发器和 DDL。
- 可视化创建和修改表结构，并生成对应数据库方言的 DDL。
- ER 图基于真实外键自动布局，也支持手工记录本地逻辑关系。
- 逻辑关系只保存在本地图谱中，不会创建或修改数据库外键。
- ER 图可导出 JSON、DBML、Mermaid 和 SVG。

### Redis 工作台

- 浏览 Redis 键空间，查看和编辑常用数据类型、TTL 与键详情。
- Workbench 支持命令执行和命令历史。
- 内嵌 Redis CLI 终端。
- 支持 Pub/Sub 订阅，可观察频道消息。
- 连接配置支持 TLS、SSH 隧道、Sentinel、Cluster 以及 URI / Cloud 配置。

### 运维、安全与更新

- 连接密码和模型 API Key 走受控存储，不进入普通配置导出、日志、事件快照或 AI 上下文。
- 应用内更新会校验签名清单与安装包摘要。
- 发布包附带 SHA-256 校验文件。
- 界面内置简体中文和 English，并支持运行时切换。

## 演示

**完整演示视频** 

https://github.com/user-attachments/assets/f6075999-a87f-4644-9d14-dbda49a7981d

## 数据库支持

各数据库的能力范围不同，具体操作见[操作手册](./docs/user-guide.md)。

| 数据库 | 当前能力 | 备份 / 恢复 | 连接与限制 |
| --- | --- | --- | --- |
| MySQL | SQL 查询、对象浏览、表结构管理、数据编辑、导入导出、原生客户端工具入口 | `mysqldump` 逻辑备份，SQL 恢复 | TLS / SSL、SSH 隧道、代理、超时、字符集 |
| TiDB | 复用 MySQL 协议链路，适配 SQL 方言、补全与表结构管理 | 复用 MySQL 逻辑备份能力 | 连接能力与 MySQL 一致 |
| PostgreSQL | SQL 查询、Schema / 对象浏览、表结构管理、数据编辑、用户与角色管理、psql 工具入口 | `pg_dump` plain SQL 备份，`psql` 恢复 | TLS、SSH、代理、Schema 作用域、超时 |
| SQLite | 本地文件连接、SQL 查询、对象浏览、表结构管理、数据编辑 | `VACUUM INTO` 完整一致快照；恢复到新文件并检查完整性与外键 | 本地文件路径 |
| 达梦 DM8 | Schema / 对象浏览、SQL 查询、表结构管理与设计、数据编辑、表数据导入 | 逻辑备份与 SQL 恢复；当前恢复面向单 Schema | 实例与 Schema 连接配置 |
| Redis | 键空间浏览、键值编辑、TTL、Workbench、CLI、Pub/Sub | 不提供数据库级备份恢复 | TLS、SSH、Sentinel、Cluster、URI / Cloud；Cluster 仅支持 DB 0 |

MongoDB、MariaDB 和 SQL Server 尚未支持。它们只出现在规划中，不应被视为当前能力。

## AI Agent

FluxDB 内置可选 AI Agent，用于理解结构、编写 SQL、排查数据和生成报表。它不是无边界的桌面助手。

- **结构发现**：搜索连接、库、Schema、表、视图和列，不默认读取全表数据。
- **有界数据读取**：只按工具契约读取有限行数或有限内容，避免把业务数据无限加入上下文。
- **授权编辑器操作**：只能修改明确授权的 SQL 编辑器范围，不会静默执行编辑器中的 SQL。
- **受控 SQL 与 Redis 执行**：`db.run`、`redis.execute` 等写入或高风险工具需要审批。
- **文件与页面上下文**：只能读取用户授权的附件、页面引用或编辑器快照。
- **BI 与报表**：可生成报告草稿、图表、表格和说明，并输出可保存的报表产物。
- **MCP 与插件**：支持 MCP Client / Server、模型绑定、插件启停、工具权限分级和用量诊断。
- **模型由用户提供**：需要自行配置模型服务和 API Key；密钥走受控存储，不进入普通日志、导出或 AI 上下文。

## 下载安装

前往[最新版本下载页](https://github.com/fluxdb-alt/fluxDB/releases/latest)，根据系统选择安装包。

| 平台 | 架构 | 安装包 |
| --- | --- | --- |
| macOS | Apple Silicon（ARM64） | `FluxDB-<版本>-macos-arm64.dmg` |
| macOS | Intel（x64） | `FluxDB-<版本>-macos-x64.dmg` |
| Windows | x64 | `FluxDB-<版本>-windows-x64-setup.exe` |
| Linux | x64 | `fluxdb_<版本>_amd64.deb` |

- **macOS**：打开 DMG，将应用拖入「应用程序」。当前应用采用 ad-hoc 签名，尚未公证；首次打开若被系统拦截，确认下载来源后，可在「系统设置 → 隐私与安全性」中允许打开。
- **Windows**：运行安装器，按提示完成安装，可选择创建桌面快捷方式。
- **Linux**：面向 Ubuntu 22.04/24.04 等 Debian 系发行版，支持 X11 / Wayland 图形桌面。下载后在所在目录执行 `sudo apt install ./fluxdb_<版本>_amd64.deb`，将 `<版本>` 替换为实际版本号。

发布包附带 `.sha256` 校验文件。Windows ARM64、Linux ARM64、AppImage、Flatpak 和 Snap 暂未提供；更多发行版与硬件环境的兼容性仍需持续验证。

MySQL / PostgreSQL 的备份恢复依赖本机 `mysqldump` / `mysql`、`pg_dump` / `psql` 客户端；基础查询和编辑不需要这些工具。

安装后，点击「添加连接」，选择数据库类型，填写连接信息并测试连接，即可开始使用。详细步骤见[操作手册](./docs/user-guide.md)（当前以 macOS 界面和快捷键为例）。

## 从源码运行

<details>
<summary>构建依赖、运行与打包步骤</summary>

### 环境要求

安装最新稳定版 [Rust 工具链](https://rustup.rs)（项目使用 Rust edition 2024），并准备对应平台的原生构建工具：

| 平台 | 构建依赖 |
| --- | --- |
| macOS | Xcode Command Line Tools，可通过 `xcode-select --install` 安装 |
| Windows | Visual Studio Build Tools 的 C++ 桌面开发工具与 Windows SDK，使用 MSVC 工具链；release 构建需要 SDK 中的 `fxc.exe` |
| Linux | C/C++ 编译器、pkg-config、CMake、Clang，以及 OpenSSL、字体、X11 / Wayland 等开发库，完整清单见 [CI 配置](./.github/workflows/ci.yml) |

### 启动应用

```bash
git clone https://github.com/fluxdb-alt/fluxDB.git
cd fluxDB
cargo run --locked -p fluxdb-desktop
```

首次编译需要下载并构建依赖，耗时较长。

### 构建与打包

```bash
cargo build --locked --release -p fluxdb-desktop
```

macOS 可使用仓库脚本生成包含动态库的 `.app` 和 `.dmg`：

```bash
./scripts/package-macos.sh
```

脚本支持 `PROFILE=debug`、`CREATE_DMG=0` 等选项，详见[打包脚本](./scripts/package-macos.sh)。Windows 安装器与 Linux DEB 的构建步骤见[发布工作流](./.github/workflows/release.yml)。

</details>

## 参与贡献

欢迎报告问题、提出建议、改进文档和提交代码。

- **报告 Bug**：先搜索[已有 Issues](https://github.com/fluxdb-alt/fluxDB/issues)，提交时注明应用版本、操作系统与架构、数据库类型与版本、复现步骤、预期和实际结果；可附脱敏后的日志或截图。
- **提出建议**：在 [Issues](https://github.com/fluxdb-alt/fluxDB/issues) 中描述使用场景和希望解决的问题。较大改动建议先讨论方案。
- **提交代码**：Fork 仓库并创建分支，保持修改聚焦，在 Pull Request 中说明改动原因与验证结果。开发约定见 [AGENTS.md](./AGENTS.md)，界面改动另请参阅 [UI 样式规范](./docs/ui-style.md)。

提交前建议运行本地门禁：

```bash
./scripts/check.sh
```

跨平台构建差异仍需在发布前通过 CI 验证。

本项目开发流程深度使用 AI 辅助，人工负责需求定义、方案审阅、测试验收和发布把关。

## 路线图

当前优先完善既有数据库的真实场景覆盖、稳定性和跨平台发布体验。后续候选方向包括：

- **MongoDB**：文档型数据源浏览与编辑。
- **MariaDB**：独立方言、系统表与备份参数适配。
- **SQL Server**：T-SQL 方言、连接协议与对象管理。
- **更多数据库**：按社区需求评估 Oracle、ClickHouse、DuckDB、Elasticsearch / OpenSearch 等方向。

路线图不代表当前版本已经支持。

## 支持与赞助

如果 FluxDB 对你有帮助，欢迎给项目一个 Star，向朋友推荐，或通过反馈问题、完善文档和贡献代码参与维护。

也欢迎通过以下方式赞助，支持持续开发和 AI 工具费用。感谢每一份支持！

| 支付宝 | 微信 |
| :---: | :---: |
| <img src="docs/sponsor/alipay.png" alt="支付宝赞助二维码" width="220"> | <img src="docs/sponsor/wechat.png" alt="微信赞助二维码" width="220"> |

## 致谢

感谢以下开源项目提供的基础能力与设计参考：

- [Zed](https://github.com/zed-industries/zed)：GPUI 生态、编辑器与原生桌面应用架构。
- [GPUI Component / gpui-kit](https://github.com/longbridge/gpui-kit)：输入框、弹窗等基础 UI 组件。
- [dbx](https://github.com/t8y2/dbx)：多数据库抽象与连接管理设计参考。
- [RedisInsight](https://github.com/RedisInsight/RedisInsight)：Redis 可视化交互设计参考。

## 许可证

FluxDB 采用 [GNU GPL v3.0 或更新版本](./LICENSE) 开源许可证。

项目版权与授权声明见 [NOTICE](./NOTICE)。
