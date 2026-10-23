#!/usr/bin/env bash
# Start an isolated SQL Server 2022 container for the ignored live tests.
#
# Usage:
#   scripts/testing/sqlserver-smoke.sh
#   scripts/testing/sqlserver-smoke.sh --keep
#   PORT=14333 scripts/testing/sqlserver-smoke.sh
#
# The script only removes the container it creates. Existing containers and
# databases are never touched. `--keep` leaves the container running and prints
# the cleanup command.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT_DIR"

IMAGE="${SQLSERVER_IMAGE:-mcr.microsoft.com/mssql/server:2022-latest}"
PORT="${PORT:-1433}"
DATABASE="${SQLSERVER_DATABASE:-fluxdb_smoke}"
SA_PASSWORD="${SQLSERVER_SA_PASSWORD:-FluxDB_Smoke_2026!Aa}"
CONTAINER="fluxdb-sqlserver-smoke-$$-$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')"
KEEP=0

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
fi
if [[ "${1:-}" == "--keep" ]]; then
    KEEP=1
elif [[ $# -gt 0 ]]; then
    echo "unknown argument: $1" >&2
    exit 2
fi

if ! command -v podman >/dev/null 2>&1; then
    echo "skip: podman is not installed" >&2
    exit 77
fi
if ! podman info >/dev/null 2>&1; then
    echo "skip: podman machine is not running (macOS: podman machine start)" >&2
    exit 77
fi

cleanup() {
    if [[ "$KEEP" -eq 1 ]]; then
        echo "container kept: $CONTAINER" >&2
        echo "cleanup: podman rm -f $CONTAINER" >&2
        return
    fi
    podman rm -f "$CONTAINER" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

# SQL Server needs at least 2 GB on macOS/Podman. Fail before pulling the image
# with an actionable message rather than letting sqlservr exit during startup.
PODMAN_MEMORY_BYTES="$(podman info --format '{{.Host.MemTotal}}' 2>/dev/null || echo 0)"
if [[ "$PODMAN_MEMORY_BYTES" =~ ^[0-9]+$ ]] && (( PODMAN_MEMORY_BYTES > 0 && PODMAN_MEMORY_BYTES < 2147483648 )); then
    echo "skip: Podman machine has less than 2 GB RAM; SQL Server will not start" >&2
    exit 77
fi

MAX_CONNECTIONS="${SQLSERVER_MAX_CONNECTIONS:-50}"
MAX_MEMORY_MB="${SQLSERVER_MAX_MEMORY_MB:-1536}"

echo "starting $IMAGE as $CONTAINER on 127.0.0.1:$PORT" >&2
podman run -d \
    --name "$CONTAINER" \
    --replace \
    -e ACCEPT_EULA=Y \
    -e MSSQL_PID=Developer \
    -e MSSQL_SA_PASSWORD="$SA_PASSWORD" \
    -e MSSQL_MEMORY_LIMIT_MB="$MAX_MEMORY_MB" \
    -p "127.0.0.1:${PORT}:1433" \
    "$IMAGE" >/dev/null

wait_for_sqlserver() {
    local deadline=$((SECONDS + 180))
    while (( SECONDS < deadline )); do
        if podman exec "$CONTAINER" /opt/mssql-tools18/bin/sqlcmd \
            -S localhost -U sa -P "$SA_PASSWORD" -C -Q "SELECT 1" >/dev/null 2>&1; then
            return 0
        fi
        if ! podman container exists "$CONTAINER" >/dev/null 2>&1; then
            echo "SQL Server container exited before becoming ready" >&2
            podman logs --tail 80 "$CONTAINER" >&2 || true
            return 1
        fi
        sleep 2
    done
    echo "SQL Server did not become ready within 180s" >&2
    podman logs --tail 80 "$CONTAINER" >&2 || true
    return 1
}

sqlcmd() {
    podman exec "$CONTAINER" /opt/mssql-tools18/bin/sqlcmd \
        -S localhost -U sa -P "$SA_PASSWORD" -C -b -Q "$1"
}

wait_for_sqlserver

echo "creating schema sample in $DATABASE" >&2
sqlcmd "
IF DB_ID(N'${DATABASE}') IS NULL CREATE DATABASE [${DATABASE}];
"
sqlcmd "
USE [${DATABASE}];
IF OBJECT_ID(N'dbo.FLUXDB_SMOKE_CHILD', N'U') IS NOT NULL DROP TABLE dbo.FLUXDB_SMOKE_CHILD;
IF OBJECT_ID(N'dbo.FLUXDB_SMOKE_PARENT', N'U') IS NOT NULL DROP TABLE dbo.FLUXDB_SMOKE_PARENT;
IF OBJECT_ID(N'dbo.FLUXDB_SMOKE_VIEW', N'V') IS NOT NULL DROP VIEW dbo.FLUXDB_SMOKE_VIEW;
CREATE TABLE dbo.FLUXDB_SMOKE_PARENT (
    id int IDENTITY(1,1) NOT NULL CONSTRAINT PK_FLUXDB_SMOKE_PARENT PRIMARY KEY,
    name nvarchar(80) NOT NULL,
    amount decimal(18,4) NULL,
    created_at datetime2(3) NULL
);
CREATE TABLE dbo.FLUXDB_SMOKE_CHILD (
    id int NOT NULL CONSTRAINT PK_FLUXDB_SMOKE_CHILD PRIMARY KEY,
    parent_id int NOT NULL,
    note nvarchar(200) NULL,
    CONSTRAINT FK_FLUXDB_SMOKE_CHILD_PARENT FOREIGN KEY (parent_id)
        REFERENCES dbo.FLUXDB_SMOKE_PARENT(id)
);
CREATE VIEW dbo.FLUXDB_SMOKE_VIEW AS
    SELECT p.id, p.name, c.note
    FROM dbo.FLUXDB_SMOKE_PARENT p
    LEFT JOIN dbo.FLUXDB_SMOKE_CHILD c ON c.parent_id = p.id;
INSERT INTO dbo.FLUXDB_SMOKE_PARENT (name, amount, created_at) VALUES
    (N'alpha', 12.3400, '2026-01-02T03:04:05.123'),
    (N'中文', 56.7800, '2026-02-03T04:05:06.456');
INSERT INTO dbo.FLUXDB_SMOKE_CHILD (id, parent_id, note) VALUES (10, 1, N'child');
"

echo
echo "SQL Server smoke database is ready."
echo "export FLUXDB_MSSQL_SMOKE='127.0.0.1:${PORT}:sa:${SA_PASSWORD}'"
echo "export FLUXDB_MSSQL_DATABASE='${DATABASE}'"
echo
echo "Run live tests serially:"
echo "  FLUXDB_PROFILE=dev FLUXDB_MSSQL_SMOKE='127.0.0.1:${PORT}:sa:${SA_PASSWORD}' FLUXDB_MSSQL_DATABASE='${DATABASE}' \\"
echo "    cargo test --locked -p fluxdb-connectors --test sqlserver_live -- --ignored --test-threads=1 --nocapture"
echo "  FLUXDB_PROFILE=dev FLUXDB_MSSQL_SMOKE='127.0.0.1:${PORT}:sa:${SA_PASSWORD}' FLUXDB_MSSQL_DATABASE='${DATABASE}' \\"
echo "    cargo test --locked -p fluxdb-app --test sqlserver_table_actions_live -- --ignored --test-threads=1 --nocapture"
