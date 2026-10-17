# FluxDB

**开源、跨平台的原生数据库客户端与 SQL 编辑器。**

在一个桌面应用中连接 MySQL、TiDB、PostgreSQL、SQLite 和 Redis，编写 SQL、编辑数据、管理表结构与浏览键值。

基于 Rust + GPUI 构建，不依赖 Electron 或 WebView，支持 macOS、Windows 和 Linux。

**简体中文 | [English](./README.en.md)**

[![Release](https://img.shields.io/github/v/release/fluxdb-alt/fluxDB)](https://github.com/fluxdb-alt/fluxDB/releases/latest)
[![CI](https://github.com/fluxdb-alt/fluxDB/actions/workflows/ci.yml/badge.svg)](https://github.com/fluxdb-alt/fluxDB/actions/workflows/ci.yml)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue)](./LICENSE)
![Platforms](https://img.shields.io/badge/平台-macOS%20%7C%20Windows%20%7C%20Linux-blue)

[下载最新版本](https://github.com/fluxdb-alt/fluxDB/releases/latest) · [功能特性](#功能特性) · [安装说明](#下载安装) · [操作手册](./docs/user-guide.md) · [反馈问题](https://github.com/fluxdb-alt/fluxDB/issues)


**演示视频** · 2 分 46 秒 · MP4，约 8.6 MB

https://github.com/user-attachments/assets/f6075999-a87f-4644-9d14-dbda49a7981d

> 早期版本，当前界面主要为中文，功能与兼容性尚未经过广泛的真实场景验证。建议先连接测试库；操作重要数据前，请先备份。

## 功能特性

- **SQL 编辑器**：语法高亮、代码折叠、关键字与表列补全、SQL 格式化和查询历史。
- **数据浏览与编辑**：分页浏览、筛选数据、编辑并回写结果，支持数据导出与 SQL 文件执行。
- **表结构管理**：查看列信息和 DDL，创建、修改数据表。
- **ER 关系图**：浏览数据库外键，手动记录本地逻辑关系，并从单表展开关联视图。本地逻辑关系不会创建或修改数据库外键。
- **Redis 工作台**：浏览与编辑键值，查看键详情，通过内嵌终端执行命令。
- **多连接工作区**：在侧边栏管理连接与数据库对象，使用标签页切换工作内容，提供明暗主题。

### 数据库支持

各数据库的功能范围有所不同，具体操作见[操作手册](./docs/user-guide.md)。

| 数据库 | 主要能力 |
| --- | --- |
| MySQL / TiDB | 连接管理、SQL 查询、表结构管理、数据编辑、导出与备份 |
| PostgreSQL | SQL 查询、Schema 与对象浏览、表结构管理、数据编辑、用户与角色权限管理、原生客户端工具 |
| SQLite | 本地数据库连接、SQL 查询、表结构浏览与数据编辑 |
| Redis | 连接总览、键值浏览与编辑、工作台与命令终端 |

## 下载安装

前往 **[最新版本下载页](https://github.com/fluxdb-alt/fluxDB/releases/latest)**，在 Assets 中选择对应平台的安装包。版本变化与更新说明见 [Releases](https://github.com/fluxdb-alt/fluxDB/releases)。

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

Rust 修改提交前运行：

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
```

UI 行为修改还需启动应用验证主窗口及相关交互。

本项目代码由 AI 生成，人工负责需求定义与验收，包括审阅变更、运行测试和检查界面效果。

## 路线图

### 已完成

- **MySQL**：连接管理（TLS/SSL、SSH 隧道、代理、超时与字符集等选项）、SQL 编辑与执行、库表与数据浏览编辑、表结构管理、结果导出，以及 mysqldump 备份与恢复、mysql 客户端工具入口。
- **TiDB**：复用 MySQL 协议与驱动，已适配 TiDB 的 SQL 方言、补全与结构管理，与 MySQL 一同验证。
- **PostgreSQL**：SQL 查询、Schema 与对象浏览、表结构管理、数据编辑、用户与角色权限管理，以及 psql 原生客户端工具。
- **SQLite**：本地数据库文件连接、SQL 查询、表结构浏览与数据编辑。
- **Redis**：连接总览、键空间浏览、键值与 TTL 编辑、工作台与内嵌命令终端。
- **AI Agent**：对话式智能体，可编写、解释并执行 SQL，探索数据库与 Redis、读取文件、做 BI 分析并生成报表产物；提供工具权限分级与写入审批、MCP Client / Server、模型与插件配置。

### 还未接入

以下为规划方向，尚未完成的功能不代表当前版本已经支持。

- **国际化与多语言**：界面文案目前固定为简体中文，计划抽出统一文案层并支持运行时语言切换，先覆盖简体中文与 English。
- **MongoDB**：文档型数据源，计划支持连接管理、集合与文档浏览、文档编辑与索引查看。
- **MariaDB**：协议兼容 MySQL，但系统表、语法与备份参数存在差异，需完成版本识别与方言适配后再作为独立数据源接入。
- **SQL Server**：T-SQL 方言与连接协议不同，需要接入驱动，并补齐对象浏览与数据编辑能力。
- **更多数据库**：按社区需求排序推进候选数据源（如 Oracle、ClickHouse、DuckDB、Elasticsearch / OpenSearch 等），复用现有 Connector 分层、SQL 方言与 AI 工具抽象；每个数据源需补齐连接管理、对象浏览、数据浏览与编辑、导入导出与备份恢复等能力后，才计入「已完成」。

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
