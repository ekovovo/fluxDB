## New features

- Filter conditions now support bidirectional editing between the form and SQL input. Form values populate the input automatically, while typed conditions can be parsed back into editable rules with AND/OR connectors, parenthesized groups and sort rules preserved.
- Condition input includes search-condition completion and a highlighted SQL preview for building and adjusting complex filter expressions.
- Sort rules now use compact pills with field changes, ascending/descending toggles, per-rule removal, clear/undo, and six-dot drag handles for changing priority.

## Improvements

- The expanded filter area now scrolls independently while sort and apply actions remain fixed. Its height is capped at 35% of the data region, so many conditions no longer cover the action buttons.
- The Form/Input switch is shown only while filter editing is expanded. Collapsing preserves conditions, the current mode and applied filter results.
- Multiple conditions, nested parentheses and per-row AND/OR connectors are supported. Complex filter SQL can round-trip between input and form editing.
- MySQL, TiDB, PostgreSQL, SQL Server, SQLite and Dameng DM8 now apply dialect-specific behavior for type casts, operators, identifiers, empty strings and `LIKE` wildcard escaping.
- The sort drag preview follows light and dark themes, using a six-dot handle and theme colors instead of an opaque dark card.
- The field detail drawer now has a 220px minimum height, keeping metadata, the value editor and save actions visible when compressed.
- In-app updates show the download percentage, downloaded size and total size, with localized release notes in the update dialog.

## Fixes

- Fixed panics when filter SQL parsing encountered a UTF-8 character boundary in Chinese and other multibyte text.
- Fixed related character-boundary risks in SQL editor completion, incremental diagnostics, statement lookup and parameter replacement.
- Fixed lost structure when complex nested conditions returned to the form, `LIKE` conditions that could not be restored, and incorrect operator display.

## Validation

- Passed `cargo fmt --all -- --check`, `cargo check --workspace --locked` and `cargo clippy --workspace --all-targets --locked`.
- Passed filter-dialect, condition round-trip, multibyte text, nested/composite condition unit tests, plus headless production-view interaction and drag-sorting E2E tests.
- Live connection acceptance for PostgreSQL, SQL Server and Dameng is outside this automated gate; dialect SQL is covered by database-specific unit tests.
