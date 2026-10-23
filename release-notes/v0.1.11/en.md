## New features

- Added SQL Server support with SQL Server authentication, TLS, database/schema/object browsing, T-SQL queries, table design and data editing, table import/export, logins, database users, role membership and explicit permissions, and object-level logical backup/restore.
- Added SQL Server AI page context and controlled execution: read-only queries and object descriptions work within bounded scopes; writes and high-risk operations require approval.
- Added SQL Server stored-procedure/function object entries, T-SQL completion, and basic database creation/deletion.

## Improvements

- Bare table-name completion now matches across schemas and continues to the next qualifier; fixed completion offsets for SQL containing UTF-8 characters.
- Expanded dialect-specific table-designer types and adaptive column options, added on-demand enum/custom-type editing, and validated length and precision.
- Unified PostgreSQL/MySQL column type display and table-design parsing while preserving opaque type parameters.
- Compacted query-summary metadata columns, improved long column-comment wrapping, and grouped Redis tabs by connection as top-level workspaces.
- Failed-SQL AI repair now locates the editor statement from the actual execution source, avoiding wrong locations after SQL rewrites.

## Known limitations

- SQL Server's first release does not yet implement Windows integrated authentication, Azure AD, contained database users, column-level permission editing, user-defined server roles, SQL Browser/named-instance auto-discovery, or database-level physical backup.
- Object-level logical backup/restore is not a substitute for database-level disaster recovery. See the README's Database support table for the full boundary.

## Validation

- Passed `cargo fmt --all`, desktop-crate `cargo check`, and `cargo clippy --all-targets` (see the local gate results before this release).
- Passed affected-module unit tests and desktop production-view E2E. Tests requiring a live SQL Server are not part of this release gate; validate connections, permissions, and backup/restore against the target environment.
