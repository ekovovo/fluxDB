## New features

- Added Oracle support with Service Name / SID connections and an optional Wallet. Objects are organized by schema after connecting, with tables, views, synonyms and other object types browsable from the tree.
- Oracle supports SQL queries with dialect-aware completion and cancellation, table structure viewing and design, data editing, filtering and sorting, and local logical export (DDL plus paged data).
- Oracle is wired into AI page context and controlled execution: `SELECT … FOR UPDATE`, sequence `NEXTVAL`, `dbms_*` / `utl_*` members, objects reached through synonyms, views or `@dblink`, and `EXPLAIN PLAN FOR` are all treated as writes and require approval.
- The user and permission panel now has five tabs: General / Roles / System privileges / Object privileges / SQL preview. For Oracle and Dameng, each tab opens with a type-locked grant editor, and multiple grants can be queued before a single save.
- Oracle supports creating accounts, changing passwords, lock/unlock, tablespaces and quotas, profiles and login grants. "New schema" opens the create-account form instead of producing a random-password account that cannot log in.
- Dameng DM8 reuses the same account panel with dialect-specific SQL such as `CREATE SESSION` and `ALTER USER … PROFILE`.

## Improvements

- User administration is split into per-database panels (MySQL / PostgreSQL / SQL Server / Oracle) with a shared shell and tab strip, so adding a database no longer touches other implementations.
- Command dispatch is split by connection, tab lifecycle and user administration, reducing the macOS debug-build thread stack from roughly 287 KiB to 90 KiB without changing command behavior or completion invalidation.
- Oracle foreign keys and completion columns are read once per owner instead of one connection per table, and the batch reads honor cancellation before connecting, between batches and after the query returns.
- Object-tree row counts come from `ALL_TABLES.NUM_ROWS` statistics, avoiding full table scans.
- Reloading table data now masks only the table area, keeping the toolbar, filter panel and SQL preview visible.

## Fixes

- Fixed Oracle import failures with `ORA-01861` by pinning ISO date formats (`NLS_DATE_FORMAT` and related settings) on each session.
- Fixed Oracle table copy always reporting that the source table DDL was not loaded.
- Fixed the Explain button sending statements that always fail to parse on Oracle and SQL Server; the toolbar button and shortcut are now disabled for those dialects.
- Fixed Oracle account changes reporting no progress information on failure; the message now includes how many statements already ran.

## Validation

- Passed `cargo fmt --all -- --check`, `cargo check --workspace --locked` and `cargo clippy --workspace --all-targets --locked`.
- Passed Oracle dialect completion, read-only policy and permission-constraint unit tests, five-tab toolbar and one-shot save flow tests, and a headless E2E for the reload mask area.
- Oracle live tests (import regression, batched metadata) live in `crates/fluxdb-connectors/tests/oracle_live.rs` and require a local Oracle instance; they are outside the automated gate.
