# FluxDB

**A native, cross-platform database client and SQL editor designed for high performance and low overhead: query, edit data, manage schemas and debug Redis in one window.**

FluxDB is built with Rust and GPUI, without Electron or a WebView. It brings MySQL, PostgreSQL, SQL Server, SQLite, TiDB, Dameng DM8 and Redis into one desktop workflow, with a permission-aware AI Agent.

**English | [简体中文](./README.md)**

[![Release](https://img.shields.io/github/v/release/fluxdb-alt/fluxDB)](https://github.com/fluxdb-alt/fluxDB/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/fluxdb-alt/fluxDB/ci.yml)](https://github.com/fluxdb-alt/fluxDB/actions/workflows/ci.yml)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue)](./LICENSE)
![Platforms](https://img.shields.io/badge/Platforms-macOS%20%7C%20Windows%20%7C%20Linux-blue)

[Download the latest release](https://github.com/fluxdb-alt/fluxDB/releases/latest) · [Latest changes](#latest-changes) · [Features](#core-capabilities) · [Database support](#database-support) · [User guide](./docs/user-guide.md) · [Report an issue](https://github.com/fluxdb-alt/fluxDB/issues)

![FluxDB SQL editor with schema completion](docs/screenshots/sql_editor_adapter.png)



## Latest changes

### v0.1.12

- **Bidirectional filter form and SQL input**: switch between form and condition-input editing in either direction. Form values populate the input automatically, and typed conditions can be parsed back into editable form rules while preserving groups, AND/OR combinations and sort rules.
- **All SQL data sources**: MySQL, TiDB, PostgreSQL, SQL Server, SQLite and Dameng DM8 apply their own dialect behavior for types, operators, identifiers and `LIKE` wildcard escaping.
- **Complex conditions**: edit multiple rules, nested parentheses and per-row AND/OR connectors; condition input includes completion and a synchronized highlighted SQL preview.
- **Filter and sort interaction**: the mode switch appears only while filters are expanded; conditions scroll while sort and apply actions stay fixed, with the expanded area limited to 35% of the data region. Sort rules support field changes, direction, removal, clear/undo and drag reordering by a six-dot handle.
- **Field detail sizing**: the detail drawer now has a 220px minimum height so metadata, the value editor and save actions remain visible while resizing.
- **Stability**: fixed UTF-8 character-boundary crashes in filter parsing, SQL editor completion and diagnostic positioning, with cross-dialect, nested-condition and round-trip coverage.
- **Update experience**: in-app updates show the download percentage, downloaded size and total size, and display localized release notes in the update dialog.

[Full release notes](./release-notes/v0.1.12/en.md) · [All releases](https://github.com/fluxdb-alt/fluxDB/releases)

### v0.1.11

- **SQL Server support**: SQL authentication, TLS, database/schema/object browsing, T-SQL queries, table design and data editing, table import/export, logins, database users, role membership and explicit permissions, object-level logical backup/restore, and AI page context with controlled execution.
- **SQL Server first-release limits**: Windows integrated authentication, Azure AD, contained database users, column-level permission editing, user-defined server roles, automatic named-instance discovery, and database-level physical backup are not implemented yet. See [Database support](#database-support).
- **SQL editor and completion**: bare table-name completion works across schemas and continues to the next qualifier; completion offsets are fixed for UTF-8 SQL.
- **Table designer**: expanded dialect-specific types and adaptive column options, on-demand enum/custom-type editing, validated lengths and precision, and consistent PostgreSQL/MySQL type display.
- **Results and Redis workspace**: query-summary metadata columns are more compact, long column comments wrap correctly, and Redis tabs are grouped by connection as top-level workspaces.
- **AI repair targeting**: failed-SQL repair now locates the editor statement using the actual execution source, so it remains correct after SQL rewriting.

## Why FluxDB

- **One workspace for multiple data sources**: manage relational databases and Redis side by side.
- **From query to write**: SQL editing, editable result sets, table design, import/export, backup/restore and ER diagrams.
- **Native architecture, performance first**: built with Rust + GPUI, without Electron or a WebView, with low overhead and responsive interaction as design goals.
- **AI with explicit boundaries**: bounded reads, authorized editor edits and approval for write or high-risk tools.

## Core capabilities

### SQL and data

- Syntax highlighting, code folding, keyword/table/column/function completion, SQL formatting and query history.
- Paginated browsing, filters, sorting, row details and cell details.
- Editable result sets with writes back to the database and safeguards for dangerous actions.
- Import TXT, CSV, JSON and XML.
- Export SQL, TXT, CSV, JSON and XML.
- Execute existing `.sql` files.

### Schema and ER diagrams

- Inspect columns, indexes, foreign keys, triggers and DDL.
- Create and modify tables visually, with DDL generated for the target database dialect.
- Layout ER diagrams from real foreign keys, with optional local logical relationships.
- Local logical relationships never create or modify database foreign keys.
- Export ER diagrams as JSON, DBML, Mermaid or SVG.

### Redis workbench

- Browse the keyspace and inspect or edit common value types, TTL and key details.
- Run commands with command history in the Workbench.
- Use the embedded Redis CLI terminal.
- Observe channel messages with Pub/Sub subscriptions.
- Connect with TLS, SSH tunnels, Sentinel, Cluster, URI or Cloud configuration.

### Operations, security and updates

- Connection passwords and model API keys use controlled storage and stay out of ordinary exports, logs, event snapshots and AI context.
- In-app updates verify the signed manifest and package checksum.
- Release packages include SHA-256 checksums.
- The UI supports English and Simplified Chinese with runtime switching.

## Demo

**Full demo** · 2 min 46 sec · MP4, about 8.6 MB

https://github.com/user-attachments/assets/f6075999-a87f-4644-9d14-dbda49a7981d

## Database support

Capabilities vary by database. See the [user guide](./docs/user-guide.md) for details.

| Database | Current capabilities | Backup / restore | Connections and limits | Not yet supported |
| --- | --- | --- | --- | --- |
| MySQL | SQL queries, object browsing, table management, data editing, import/export, native client tool entry | `mysqldump` logical backup; SQL restore | TLS/SSL, SSH tunnel, proxy, timeouts, charset | - |
| TiDB | Reuses the MySQL protocol path with TiDB SQL dialect, completion and schema management | Reuses MySQL logical backup | Same connection features as MySQL | - |
| PostgreSQL | SQL queries, schema/object browsing, table management, data editing, users and roles, psql tool entry | `pg_dump` plain SQL; `psql` restore | TLS, SSH, proxy, schema scope, timeouts | - |
| SQL Server | T-SQL queries, database/schema/object browsing, table design and data editing, table import/export, logins, database users, role membership and explicit permissions, object-level logical backup/restore, AI page context and controlled execution | Object-level logical SQL backup and restore | SQL Server authentication, TLS, explicit ports; named instances require the actual port | Windows integrated authentication, Azure AD, contained database users, column-level permission editing, user-defined server roles, SQL Browser/named-instance auto-discovery, and database-level physical `BACKUP DATABASE`. Object-level logical backup and restore is not a substitute for database-level disaster recovery. |
| SQLite | Local file connections, SQL queries, object browsing, table management, data editing | Consistent `VACUUM INTO` snapshot; restore to a new file with integrity and foreign-key checks | Local file path | - |
| Dameng DM8 | Schema/object browsing, SQL queries, table design and management, data editing, table import | Logical backup and SQL restore; restore currently targets a single schema | Instance and schema connection settings | - |
| Redis | Keyspace browsing, value editing, TTL, Workbench, CLI, Pub/Sub | No database-level backup/restore | TLS, SSH, Sentinel, Cluster, URI/Cloud; Cluster uses DB 0 only | - |

MongoDB and MariaDB are not currently supported. They are roadmap candidates, not current capabilities.

## AI Agent

FluxDB includes an optional AI Agent for understanding schemas, writing SQL, investigating data and generating reports. It is not an unrestricted desktop assistant.

- **Schema discovery**: search connections, databases, schemas, tables, views and columns without reading whole tables by default.
- **Bounded reads**: read only the rows or content allowed by each tool contract.
- **Authorized editor actions**: edit only an explicitly authorized SQL range and never execute it implicitly.
- **Controlled SQL and Redis execution**: write or high-risk tools such as `db.run` and `redis.execute` require approval.
- **Scoped context**: read only authorized attachments, page references or editor snapshots.
- **BI and reports**: generate report drafts, charts, tables and explanations as saveable artifacts.
- **MCP and plugins**: configure MCP Client/Server, model bindings, plugins, tool permissions and usage diagnostics.
- **User-provided models**: configure your own model service and API key. Secrets use controlled storage and do not enter ordinary logs, exports or AI context.

## Download and install

Open the [latest release](https://github.com/fluxdb-alt/fluxDB/releases/latest) and choose the package for your platform.

| Platform | Architecture | Package |
| --- | --- | --- |
| macOS | Apple Silicon (ARM64) | `FluxDB-<version>-macos-arm64.dmg` |
| macOS | Intel (x64) | `FluxDB-<version>-macos-x64.dmg` |
| Windows | x64 | `FluxDB-<version>-windows-x64-setup.exe` |
| Linux | x64 | `fluxdb_<version>_amd64.deb` |

- **macOS**: open the DMG and drag FluxDB into Applications. The app is currently ad-hoc signed and not notarized. If macOS blocks the first launch, verify the download source and allow it in System Settings → Privacy & Security.
- **Windows**: run the installer and optionally create a desktop shortcut.
- **Linux**: targets Debian-based distributions such as Ubuntu 22.04/24.04 with X11 or Wayland. Run `sudo apt install ./fluxdb_<version>_amd64.deb`.

Release packages include `.sha256` checksums. Windows ARM64, Linux ARM64, AppImage, Flatpak and Snap are not currently available.

MySQL and PostgreSQL backup/restore depend on the local `mysqldump`/`mysql` and `pg_dump`/`psql` clients. Basic queries and editing do not require them.

After installation, choose “Add connection”, select a database type, enter the connection details and test the connection. See the [user guide](./docs/user-guide.md); it currently uses Chinese screenshots and macOS shortcuts.

## Run from source

<details>
<summary>Build dependencies, running and packaging</summary>

### Prerequisites

Install the latest stable [Rust toolchain](https://rustup.rs) (the project uses Rust edition 2024) and the native build tools for your platform:

| Platform | Build dependencies |
| --- | --- |
| macOS | Xcode Command Line Tools, available through `xcode-select --install` |
| Windows | Visual Studio Build Tools with Desktop development with C++ and the Windows SDK, using the MSVC toolchain; release builds require the SDK's `fxc.exe` |
| Linux | C/C++ compiler, pkg-config, CMake, Clang, OpenSSL, font and X11/Wayland development libraries; see the [CI configuration](./.github/workflows/ci.yml) for the complete list |

### Run

```bash
git clone https://github.com/fluxdb-alt/fluxDB.git
cd fluxDB
cargo run --locked -p fluxdb-desktop
```

The first build downloads and compiles dependencies and may take a while.

### Build and package

```bash
cargo build --locked --release -p fluxdb-desktop
```

On macOS, generate an `.app` and `.dmg` containing the required dynamic libraries:

```bash
./scripts/package-macos.sh
```

The script supports options such as `PROFILE=debug` and `CREATE_DMG=0`; see the [packaging script](./scripts/package-macos.sh). Windows installer and Linux DEB steps are defined in the [release workflow](./.github/workflows/release.yml).

</details>

## Contributing

Issues, feature proposals, documentation improvements and code contributions are welcome.

- **Report a bug**: search [existing issues](https://github.com/fluxdb-alt/fluxDB/issues) first, then include the app version, operating system and architecture, database type and version, reproduction steps, expected and actual results, and sanitized logs or screenshots where useful.
- **Suggest a feature**: describe the use case and the problem it solves. Discuss larger changes before implementation.
- **Contribute code**: fork the repository, create a focused branch and explain the reason and validation results in the pull request. See [AGENTS.md](./AGENTS.md) for development conventions and the [UI style guide](./docs/ui-style.md) for interface changes (both in Chinese).

Run the local quality gate before submitting:

```bash
./scripts/check.sh
```

Cross-platform build differences still require CI verification before release.

AI is used extensively in the development workflow. Humans own requirements, review, validation and release approval.

## Roadmap

Current priorities are real-world coverage for existing databases, stability and the cross-platform release experience. Candidate directions include:

- **SQL Server follow-up**: Windows integrated authentication and Azure AD, contained database users, column-level permissions, user-defined server roles, named-instance auto-discovery, and database-level physical backup/restore.
- **MongoDB**: document browsing, querying and editing.
- **MariaDB**: independent dialect, system tables and backup parameter support.
- **More databases**: evaluate Oracle, ClickHouse, DuckDB, Elasticsearch/OpenSearch and others based on community demand.

The roadmap does not imply current support.

## Support and sponsorship

If FluxDB is useful to you, consider starring the repository, sharing it with others, or contributing through issues, documentation and code.

You can also sponsor ongoing development and AI tooling costs.

| Alipay | WeChat |
| :---: | :---: |
| <img src="docs/sponsor/alipay.png" alt="Alipay sponsorship QR code" width="220"> | <img src="docs/sponsor/wechat.png" alt="WeChat sponsorship QR code" width="220"> |

## Acknowledgements

- [Zed](https://github.com/zed-industries/zed): GPUI ecosystem, editor and native desktop architecture.
- [GPUI Component / gpui-kit](https://github.com/longbridge/gpui-kit): reusable UI components.
- [dbx](https://github.com/t8y2/dbx): multi-database abstraction and connection-management design reference.
- [RedisInsight](https://github.com/RedisInsight/RedisInsight): Redis visualization and interaction design reference.

## License

FluxDB is licensed under the [GNU GPL v3.0 or later](./LICENSE).

See [NOTICE](./NOTICE) for copyright and attribution details.
