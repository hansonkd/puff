#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BENCH_DIR="$ROOT_DIR/bench"
RESULTS_DIR="${RESULTS_DIR:-$BENCH_DIR/results}"
PUFF_PORT="${PUFF_PORT:-7788}"
STRAWBERRY_PORT="${STRAWBERRY_PORT:-7790}"
WRK_THREADS="${WRK_THREADS:-4}"
WRK_CONNECTIONS="${WRK_CONNECTIONS:-64}"
WRK_DURATION="${WRK_DURATION:-10s}"
ITEM_COUNT="${ITEM_COUNT:-64}"
PUFF_BIN="${PUFF_BIN:-$ROOT_DIR/target/release/puff}"
PYTHON_BIN="${PYTHON_BIN:-$ROOT_DIR/.venv/bin/python}"
PUFF_PID=""
STRAWBERRY_PID=""

mkdir -p "$RESULTS_DIR"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$RESULTS_DIR/${RUN_ID}_graphql_compare"
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

build_body() {
    "$PYTHON_BIN" - "$1" "$ITEM_COUNT" <<'PY'
import json
import sys

kind = sys.argv[1]
count = int(sys.argv[2])

if kind == "simple":
    payload = {"query": "query Bench { hello_world }"}
elif kind == "nested":
    payload = {
        "query": "query Bench { " + " ".join(f"field_{ix}" for ix in range(count)) + " }",
    }
else:
    raise SystemExit(f"unknown query kind: {kind}")

print(json.dumps(payload, separators=(',', ':')))
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

SIMPLE_BODY="$(build_body simple)"
NESTED_BODY="$(build_body nested)"

echo "Building Puff release binary..."
(cd "$ROOT_DIR" && cargo build --release --bin puff)

echo "Starting Puff GraphQL on 127.0.0.1:$PUFF_PORT..."
VIRTUAL_ENV="$ROOT_DIR/.venv" \
PUFF_CONFIG="$BENCH_DIR/puff_graphql_bench.toml" \
    "$PUFF_BIN" serve --bind "127.0.0.1:$PUFF_PORT" >"$RUN_DIR/puff_server.log" 2>&1 &
PUFF_PID=$!
wait_for_graphql "http://127.0.0.1:$PUFF_PORT/graphql" "$SIMPLE_BODY"

echo "Starting Strawberry GraphQL on 127.0.0.1:$STRAWBERRY_PORT..."
PYTHONPATH="$ROOT_DIR${PYTHONPATH:+:$PYTHONPATH}" \
    "$PYTHON_BIN" -m uvicorn bench.strawberry_graphql_bench_schema:strawberry_app \
    --host 127.0.0.1 --port "$STRAWBERRY_PORT" >"$RUN_DIR/strawberry_server.log" 2>&1 &
STRAWBERRY_PID=$!
wait_for_graphql "http://127.0.0.1:$STRAWBERRY_PORT" "$SIMPLE_BODY"

run_wrk "puff-simple" "http://127.0.0.1:$PUFF_PORT/graphql" "$SIMPLE_BODY" "$RUN_DIR/puff_simple.txt"
run_wrk "strawberry-simple" "http://127.0.0.1:$STRAWBERRY_PORT" "$SIMPLE_BODY" "$RUN_DIR/strawberry_simple.txt"
run_wrk "puff-nested" "http://127.0.0.1:$PUFF_PORT/graphql" "$NESTED_BODY" "$RUN_DIR/puff_nested.txt"
run_wrk "strawberry-nested" "http://127.0.0.1:$STRAWBERRY_PORT" "$NESTED_BODY" "$RUN_DIR/strawberry_nested.txt"

echo
echo "Results written to $RUN_DIR"
