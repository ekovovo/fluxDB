# FluxDB

**An open-source, cross-platform native database client and SQL editor.**

Connect to MySQL, TiDB, PostgreSQL, SQLite, Dameng DM8, and Redis in one desktop app to write SQL, edit data, manage table structures, and browse key-value data.

Built with Rust + GPUI, without Electron or a WebView. Available for macOS, Windows, and Linux.

**English | [简体中文](./README.md)**

[![Release](https://img.shields.io/github/v/release/fluxdb-alt/fluxDB)](https://github.com/fluxdb-alt/fluxDB/releases/latest)
[![CI](https://github.com/fluxdb-alt/fluxDB/actions/workflows/ci.yml/badge.svg)](https://github.com/fluxdb-alt/fluxDB/actions/workflows/ci.yml)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue)](./LICENSE)
![Platforms](https://img.shields.io/badge/Platforms-macOS%20%7C%20Windows%20%7C%20Linux-blue)

[Download the latest release](https://github.com/fluxdb-alt/fluxDB/releases/latest) · [Features](#features) · [Installation](#download-and-install) · [User Guide](./docs/user-guide.md) · [Report an issue](https://github.com/fluxdb-alt/fluxDB/issues)



**Demo video** · 2 min 46 sec · MP4, approximately 8.6 MB

https://github.com/user-attachments/assets/f6075999-a87f-4644-9d14-dbda49a7981d

> Early release. The UI supports Simplified Chinese and English, and features and compatibility have not yet been tested across a broad range of real-world scenarios. Start with a test database and back up important data before making changes.

## Features

- **SQL editor**: Syntax highlighting, code folding, keyword/table/column completion, SQL formatting, and query history.
- **Data browsing and editing**: Browse pages, filter rows, edit and write back results, export data, import table data, and execute SQL files.
- **Table management**: Inspect columns and DDL, create tables, and modify table structures.
- **ER diagrams**: Browse database foreign keys, manually record local logical relationships, and explore related tables from a single table. Local logical relationships do not create or modify database foreign keys.
- **Redis workbench**: Browse and edit values, inspect key details, and run commands in an embedded terminal.
- **Multi-connection workspace**: Manage connections and database objects in the sidebar, switch between tabs, and choose a light or dark theme.

### Database support

Available features vary by database. See the [user guide](./docs/user-guide.md) (in Chinese) for details.

| Database | Main capabilities |
| --- | --- |
| MySQL / TiDB | Connections, SQL queries, table management, data editing, export, and backup |
| PostgreSQL | SQL queries, schema and object browsing, table management, data editing, user and role administration, and native client tools |
| SQLite | Local database connections, SQL queries, table structure browsing, and data editing |
| Dameng DM8 | Connections, schema and object browsing, SQL queries, table management, data editing, table data import, and backup/restore |
| Redis | Connection overview, key-value browsing and editing, workbench, and command terminal |

## Download and install

Open the **[latest release](https://github.com/fluxdb-alt/fluxDB/releases/latest)** and choose your platform's package under Assets. See [Releases](https://github.com/fluxdb-alt/fluxDB/releases) for release notes and earlier versions.

| Platform | Architecture | Package |
| --- | --- | --- |
| macOS | Apple Silicon (ARM64) | `FluxDB-<version>-macos-arm64.dmg` |
| macOS | Intel (x64) | `FluxDB-<version>-macos-x64.dmg` |
| Windows | x64 | `FluxDB-<version>-windows-x64-setup.exe` |
| Linux | x64 | `fluxdb_<version>_amd64.deb` |

- **macOS**: Open the DMG and drag the app into Applications. The app is currently ad-hoc signed and not notarized. If macOS blocks the first launch, verify the download source and allow it in System Settings → Privacy & Security.
- **Windows**: Run the installer and follow the prompts. A desktop shortcut is optional.
- **Linux**: The DEB targets Debian-based distributions such as Ubuntu 22.04/24.04, with X11 / Wayland desktops. From the download directory, run `sudo apt install ./fluxdb_<version>_amd64.deb`, replacing `<version>` with the actual version.

Release packages include `.sha256` checksum files. Windows ARM64, Linux ARM64, AppImage, Flatpak, and Snap packages are not currently available. Compatibility with additional distributions and hardware still needs validation.

After installation, click “添加连接” (Add connection), choose a database type, enter connection details, and test the connection. Follow the [user guide](./docs/user-guide.md) for more details; it is currently in Chinese and uses macOS screenshots and shortcuts.

## Run from source

<details>
<summary>Build dependencies, running, and packaging</summary>

### Prerequisites

Install the latest stable [Rust toolchain](https://rustup.rs) (the project uses Rust edition 2024) and your platform's native build tools:

| Platform | Build dependencies |
| --- | --- |
| macOS | Xcode Command Line Tools, available through `xcode-select --install` |
| Windows | Visual Studio Build Tools with Desktop development with C++ and Windows SDK, using the MSVC toolchain; release builds require the SDK's `fxc.exe` |
| Linux | C/C++ compiler, pkg-config, CMake, Clang, and development libraries for OpenSSL, fonts, X11 / Wayland, and more; see the [CI configuration](./.github/workflows/ci.yml) for the complete list |

### Launch the app

```bash
git clone https://github.com/fluxdb-alt/fluxDB.git
cd fluxDB
cargo run --locked -p fluxdb-desktop
```

The first build takes time to download and compile dependencies.

### Build and package

```bash
cargo build --locked --release -p fluxdb-desktop
```

On macOS, use the bundled script to generate an `.app` and `.dmg` with dynamic libraries included:

```bash
./scripts/package-macos.sh
```

The script supports options such as `PROFILE=debug` and `CREATE_DMG=0`; see the [packaging script](./scripts/package-macos.sh). Windows installer and Linux DEB packaging steps are in the [release workflow](./.github/workflows/release.yml).

</details>

## Contributing

Bug reports, feature suggestions, documentation improvements, and code contributions are welcome.

- **Report a bug**: Search [existing issues](https://github.com/fluxdb-alt/fluxDB/issues) first. Include the app version, operating system and architecture, database type and version, reproduction steps, and expected versus actual behavior. Add sanitized logs or screenshots where useful.
- **Suggest a feature**: Describe your use case and the problem in [Issues](https://github.com/fluxdb-alt/fluxDB/issues). For larger changes, discuss the approach before implementation.
- **Contribute code**: Fork the repository, create a branch, and keep changes focused. Explain the reason for the change and validation results in your pull request. See [AGENTS.md](./AGENTS.md) for development conventions and the [UI style guide](./docs/ui-style.md) for interface changes (both in Chinese).

Before submitting Rust changes, run:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
```

For UI behavior changes, also launch the app and verify the main window and affected interactions.

The project's code is generated by AI. Humans define requirements and perform acceptance checks, including reviewing changes, running tests, and inspecting the UI.

## Roadmap

### Done

- **MySQL**: connection management (TLS/SSL, SSH tunnel, proxy, timeouts and charset options), SQL editing and execution, database/table and data browsing and editing, table structure management, result export, plus mysqldump backup and restore and the mysql client tool entry.
- **TiDB**: reuses the MySQL protocol and driver, with TiDB SQL dialect, completion and structure management already adapted and verified alongside MySQL.
- **PostgreSQL**: SQL queries, schema and object browsing, table structure management, data editing, user and role permission management, and the psql native client tool.
- **SQLite**: local database file connections, SQL queries, table structure browsing and data editing.
- **Dameng DM8**: instance and schema connections, object browsing, SQL queries, table structure management and design, data editing, table data import, logical backup and whole-schema restore, and semantic completion.
- **Redis**: connection overview, keyspace browsing, key value and TTL editing, workbench and the embedded command terminal.
- **AI Agent**: a conversational agent that can write, explain and run SQL, explore databases and Redis, read files, run BI analysis and produce reports; it ships tool permission tiers with write approval, page and SQL editor grants, MCP client / server, and model and plugin configuration.
- **UI internationalization**: bundled Simplified Chinese and English packs with runtime switching and user locale overrides; adding a language requires no Rust changes.
- **In-app updates**: check GitHub Releases for updates, verify the signed manifest and package checksum, then download and install.

### Not yet integrated

These are planned directions; unfinished items are not available features of the current release.

- **MongoDB**: a document data source; the plan covers connection management, collection and document browsing, document editing and index inspection.
- **MariaDB**: protocol-compatible with MySQL but different in system tables, syntax and backup parameters, so version detection and dialect adaptation are needed before it is offered as a standalone data source.
- **SQL Server**: a different connection protocol and T-SQL dialect, requiring driver integration plus object browsing and data editing support.
- **More databases**: further data sources are taken in community-demand order (for example Oracle, ClickHouse, DuckDB, Elasticsearch / OpenSearch), reusing the existing connector layers, SQL dialects and AI tool abstractions. A data source only counts as "done" once connection management, object browsing, data browsing and editing, import/export and backup/restore are all in place.

## Support & Sponsorship

If FluxDB is useful to you, consider starring the repository, sharing it with others, reporting issues, improving the docs, or contributing code.

You can also sponsor development and AI tooling costs using the QR codes below. Thank you for your support!

| Alipay | WeChat Pay |
| :---: | :---: |
| <img src="docs/sponsor/alipay.png" alt="Alipay sponsorship QR code" width="220"> | <img src="docs/sponsor/wechat.png" alt="WeChat Pay sponsorship QR code" width="220"> |

## Acknowledgments

Thanks to these open-source projects for their foundations and design references:

- [Zed](https://github.com/zed-industries/zed): The GPUI ecosystem, editor design, and native desktop architecture.
- [GPUI Component / gpui-kit](https://github.com/longbridge/gpui-kit): UI components such as inputs and dialogs.
- [dbx](https://github.com/t8y2/dbx): References for database abstractions and connection management.
- [RedisInsight](https://github.com/RedisInsight/RedisInsight): References for Redis visualization and interactions.

## License

FluxDB is licensed under the [GNU GPL v3.0 or later](./LICENSE).

See [NOTICE](./NOTICE) for the project's copyright and license notice.
