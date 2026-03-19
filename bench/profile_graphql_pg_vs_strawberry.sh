#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BENCH_DIR="$ROOT_DIR/bench"
RESULTS_DIR="${RESULTS_DIR:-$BENCH_DIR/results}"
PUFF_PORT="${PUFF_PORT:-7788}"
STRAWBERRY_PORT="${STRAWBERRY_PORT:-7790}"
POSTGRES_PORT="${POSTGRES_PORT:-15432}"
POSTGRES_CONTAINER="${POSTGRES_CONTAINER:-puff-bench-postgres}"
POSTGRES_IMAGE="${POSTGRES_IMAGE:-postgres:16-alpine}"
WRK_THREADS="${WRK_THREADS:-4}"
WRK_CONNECTIONS="${WRK_CONNECTIONS:-64}"
WRK_DURATION="${WRK_DURATION:-10s}"
ITEM_COUNT="${ITEM_COUNT:-64}"
ROW_COUNT="${ROW_COUNT:-1024}"
PUFF_BIN="${PUFF_BIN:-$ROOT_DIR/target/release/puff}"
PYTHON_BIN="${PYTHON_BIN:-$ROOT_DIR/.venv/bin/python}"
SKIP_BUILD="${SKIP_BUILD:-0}"
PUFF_PID=""
STRAWBERRY_PID=""

mkdir -p "$RESULTS_DIR"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$RESULTS_DIR/${RUN_ID}_graphql_pg_compare"
mkdir -p "$RUN_DIR"

cleanup() {
    if [[ -n "$PUFF_PID" ]] && kill -0 "$PUFF_PID" 2>/dev/null; then
        kill "$PUFF_PID" 2>/dev/null || true
        wait "$PUFF_PID" 2>/dev/null || true
    fi
    if [[ -n "$STRAWBERRY_PID" ]] && kill -0 "$STRAWBERRY_PID" 2>/dev/null; then
        kill "$STRAWBERRY_PID" 2>/dev/null || true
        wait "$STRAWBERRY_PID" 2>/dev/null || true
    fi
    docker rm -f "$POSTGRES_CONTAINER" >/dev/null 2>&1 || true
}

trap cleanup EXIT

wait_for_graphql() {
    local url="$1"
    local body="$2"
    for _ in $(seq 1 60); do
        if curl -fsS -X POST -H "Content-Type: application/json" --data "$body" "$url" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.5
    done
    echo "graphql endpoint did not become ready: $url" >&2
    return 1
}

wait_for_postgres() {
    for _ in $(seq 1 60); do
        if docker exec "$POSTGRES_CONTAINER" pg_isready -h 127.0.0.1 -U postgres -d postgres >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.5
    done
    echo "postgres did not become ready" >&2
    return 1
}

seed_postgres() {
    docker exec -i "$POSTGRES_CONTAINER" psql -h 127.0.0.1 -U postgres -d postgres >/dev/null <<SQL
CREATE TABLE IF NOT EXISTS puff_graphql_pg_bench (
    id integer PRIMARY KEY,
    value text NOT NULL
);
TRUNCATE puff_graphql_pg_bench;
INSERT INTO puff_graphql_pg_bench (id, value)
SELECT g, 'value-' || g::text
FROM generate_series(1, ${ROW_COUNT}) AS g;
SQL
}

build_body() {
    "$PYTHON_BIN" - "$1" "$ITEM_COUNT" <<'PY'
import json
import sys

kind = sys.argv[1]
count = int(sys.argv[2])

queries = {
    "puff_fused": {"query": f"query Bench {{ pg_items_fused(count: {count}) {{ id value }} }}"},
    "puff_python": {"query": f"query Bench {{ pg_items_python(count: {count}) {{ id value }} }}"},
    "strawberry": {"query": f"query Bench {{ pg_items(count: {count}) {{ id value }} }}"},
}

print(json.dumps(queries[kind], separators=(",", ":")))
PY
}

run_wrk() {
    local label="$1"
    local url="$2"
    local body="$3"
    local output="$4"
    echo "==> $label"
    GRAPHQL_BODY="$body" \
        wrk -t"$WRK_THREADS" -c"$WRK_CONNECTIONS" -d"$WRK_DURATION" --latency \
        -s "$BENCH_DIR/graphql_post.lua" "$url" | tee "$output"
}

if [[ "$SKIP_BUILD" != "1" ]]; then
    echo "Building Puff release binary..."
    (cd "$ROOT_DIR" && cargo build --release --bin puff)
fi

echo "Starting Postgres on 127.0.0.1:$POSTGRES_PORT..."
docker rm -f "$POSTGRES_CONTAINER" >/dev/null 2>&1 || true
docker run -d --rm --name "$POSTGRES_CONTAINER" \
    -e POSTGRES_USER=postgres \
    -e POSTGRES_PASSWORD=password \
    -e POSTGRES_DB=postgres \
    -p "127.0.0.1:${POSTGRES_PORT}:5432" \
    "$POSTGRES_IMAGE" >/dev/null
wait_for_postgres
seed_postgres

PUFF_FUSED_BODY="$(build_body puff_fused)"
PUFF_PYTHON_BODY="$(build_body puff_python)"
STRAWBERRY_BODY="$(build_body strawberry)"
PG_DSN="postgres://postgres:password@127.0.0.1:${POSTGRES_PORT}/postgres"

echo "Starting Puff GraphQL on 127.0.0.1:$PUFF_PORT..."
VIRTUAL_ENV="$ROOT_DIR/.venv" \
PUFF_CONFIG="$BENCH_DIR/puff_graphql_pg_bench.toml" \
PUFF_DEFAULT_POSTGRES_URL="$PG_DSN" \
    "$PUFF_BIN" serve --bind "127.0.0.1:$PUFF_PORT" >"$RUN_DIR/puff_server.log" 2>&1 &
PUFF_PID=$!
wait_for_graphql "http://127.0.0.1:$PUFF_PORT/graphql" "$PUFF_FUSED_BODY"

echo "Starting Strawberry GraphQL on 127.0.0.1:$STRAWBERRY_PORT..."
PYTHONPATH="$ROOT_DIR${PYTHONPATH:+:$PYTHONPATH}" \
BENCH_PG_DSN="$PG_DSN" \
    "$PYTHON_BIN" -m uvicorn bench.strawberry_graphql_pg_bench_schema:strawberry_app \
    --host 127.0.0.1 --port "$STRAWBERRY_PORT" >"$RUN_DIR/strawberry_server.log" 2>&1 &
STRAWBERRY_PID=$!
wait_for_graphql "http://127.0.0.1:$STRAWBERRY_PORT" "$STRAWBERRY_BODY"

run_wrk "puff-pg-fused" "http://127.0.0.1:$PUFF_PORT/graphql" "$PUFF_FUSED_BODY" "$RUN_DIR/puff_pg_fused.txt"
run_wrk "puff-pg-python" "http://127.0.0.1:$PUFF_PORT/graphql" "$PUFF_PYTHON_BODY" "$RUN_DIR/puff_pg_python.txt"
run_wrk "strawberry-pg" "http://127.0.0.1:$STRAWBERRY_PORT" "$STRAWBERRY_BODY" "$RUN_DIR/strawberry_pg.txt"

echo
echo "Results written to $RUN_DIR"
