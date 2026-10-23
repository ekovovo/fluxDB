## New features

- Added Dameng DM8 support: connections, schema and object browsing, SQL queries, table structure management and design, data editing, table data import, logical backup and whole-schema restore, plus Dameng-aware semantic completion.
- Added a table import wizard for TXT, CSV, JSON and XML with column mapping, append/truncate modes, batch progress and a message log.
- Added page-query and SQL editor edit grants for the AI Agent: find and open a table, fill visible filters and query once; rewrite an authorized SQL range with an undoable edit that never executes automatically.
- Added in-app updates with signed manifest and package checksum verification against GitHub Releases.

## Improvements

- Rebuilt SQL completion as an independent semantic engine with function signatures, lexical scopes, clause-aware keyword slots and diagnostics.
- Added dynamic AI toolbox search/load; settings changes now go through confirmed configuration proposals.
- Updated the roadmap for Dameng DM8, internationalization, in-app updates and table data import.

## Fixes

- Fixed framed-response detection, duplicate generated index columns, blocking table-designer metadata loads and object refresh issues on Dameng.
- Fixed loaded tables briefly disappearing during sidebar refresh and table context menus overflowing the viewport.
- Fixed late results overwriting editor or page state, with added concurrency and cancellation coverage.
- Fixed in-app updates rejecting wrapped signature files with an invalid Base64 error.

## Validation

- Passed local `cargo fmt`, workspace `cargo check`, and workspace `cargo clippy --all-targets`.
- Passed affected-crate tests and desktop production-view E2E; tests requiring live Dameng/MySQL/PostgreSQL remain explicitly ignored.
