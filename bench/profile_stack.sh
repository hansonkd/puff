#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BENCH_DIR="$ROOT_DIR/bench"
MODE="${1:-wsgi}"
PORT="${PORT:-7799}"
WRK_THREADS="${WRK_THREADS:-4}"
WRK_CONNECTIONS="${WRK_CONNECTIONS:-64}"
WRK_DURATION="${WRK_DURATION:-10s}"
REDIS_PORT="${REDIS_PORT:-16379}"
POSTGRES_PORT="${POSTGRES_PORT:-15432}"
REDIS_CONTAINER="${REDIS_CONTAINER:-puff-bench-redis}"
POSTGRES_CONTAINER="${POSTGRES_CONTAINER:-puff-bench-postgres}"
REDIS_IMAGE="${REDIS_IMAGE:-valkey/valkey:8}"
POSTGRES_IMAGE="${POSTGRES_IMAGE:-postgres:16-alpine}"
PUFF_BIN="${PUFF_BIN:-$ROOT_DIR/target/release/puff}"
ENABLE_PERF="${ENABLE_PERF:-0}"
PERF_SECONDS="${PERF_SECONDS:-10}"
RESULTS_DIR="${RESULTS_DIR:-$BENCH_DIR/results}"

mkdir -p "$RESULTS_DIR"

case "$MODE" in
    wsgi) CONFIG="$BENCH_DIR/puff_integration_wsgi.toml" ;;
    asgi) CONFIG="$BENCH_DIR/puff_integration_asgi.toml" ;;
    *)
        echo "usage: $0 [wsgi|asgi]" >&2
        exit 1
        ;;
esac

SERVER_LOG="$RESULTS_DIR/${MODE}_server.log"
PERF_LOG="$RESULTS_DIR/${MODE}_perf.log"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$RESULTS_DIR/${RUN_ID}_${MODE}"
mkdir -p "$RUN_DIR"

SERVER_PID=""

cleanup() {
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    docker rm -f "$REDIS_CONTAINER" "$POSTGRES_CONTAINER" >/dev/null 2>&1 || true
}

trap cleanup EXIT

wait_for_http() {
    local url="$1"
    for _ in $(seq 1 60); do
        if curl -fsS "$url" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.5
    done
    echo "server did not become ready: $url" >&2
    return 1
}

wait_for_redis() {
    for _ in $(seq 1 60); do
        if docker exec "$REDIS_CONTAINER" sh -lc 'redis-cli ping >/dev/null 2>&1 || valkey-cli ping >/dev/null 2>&1'; then
            return 0
        fi
        sleep 0.5
    done
    echo "redis did not become ready" >&2
    return 1
}

wait_for_postgres() {
    for _ in $(seq 1 60); do
        if docker exec "$POSTGRES_CONTAINER" pg_isready -U postgres -d postgres >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.5
    done
    echo "postgres did not become ready" >&2
    return 1
}

seed_redis() {
    docker exec "$REDIS_CONTAINER" sh -lc 'redis-cli SET bench:key bench-value >/dev/null 2>&1 || valkey-cli SET bench:key bench-value >/dev/null 2>&1'
}

seed_postgres() {
    docker exec -i "$POSTGRES_CONTAINER" psql -U postgres -d postgres >/dev/null <<'SQL'
CREATE TABLE IF NOT EXISTS puff_bench_lookup (
    id integer PRIMARY KEY,
    value text NOT NULL
);
INSERT INTO puff_bench_lookup (id, value)
VALUES (1, 'postgres-value')
ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value;
SQL
}

run_wrk() {
    local endpoint="$1"
    local out_file="$2"
    echo "==> $endpoint"
    wrk -t"$WRK_THREADS" -c"$WRK_CONNECTIONS" -d"$WRK_DURATION" --latency \
        "http://127.0.0.1:$PORT$endpoint" | tee "$out_file"
}

echo "Building Puff release binary..."
(cd "$ROOT_DIR" && cargo build --release --bin puff)

echo "Starting Redis on 127.0.0.1:$REDIS_PORT..."
docker rm -f "$REDIS_CONTAINER" >/dev/null 2>&1 || true
docker run -d --rm --name "$REDIS_CONTAINER" -p "127.0.0.1:${REDIS_PORT}:6379" "$REDIS_IMAGE" >/dev/null
wait_for_redis
seed_redis

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

echo "Starting Puff $MODE server on 127.0.0.1:$PORT..."
PUFF_CONFIG="$CONFIG" \
PUFF_DEFAULT_REDIS_URL="redis://127.0.0.1:${REDIS_PORT}" \
PUFF_DEFAULT_POSTGRES_URL="postgres://postgres:password@127.0.0.1:${POSTGRES_PORT}/postgres" \
"$PUFF_BIN" serve --bind "127.0.0.1:$PORT" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

wait_for_http "http://127.0.0.1:$PORT/baseline"

curl -fsS "http://127.0.0.1:$PORT/baseline" >/dev/null
curl -fsS "http://127.0.0.1:$PORT/redis" >/dev/null
curl -fsS "http://127.0.0.1:$PORT/pg" >/dev/null
curl -fsS "http://127.0.0.1:$PORT/combo" >/dev/null

if [[ "$ENABLE_PERF" == "1" ]]; then
    perf stat -d -p "$SERVER_PID" -- sleep "$PERF_SECONDS" >"$PERF_LOG" 2>&1 &
fi

run_wrk "/baseline" "$RUN_DIR/baseline.txt"
run_wrk "/redis" "$RUN_DIR/redis.txt"
run_wrk "/pg" "$RUN_DIR/pg.txt"
run_wrk "/combo" "$RUN_DIR/combo.txt"

echo
echo "Results written to $RUN_DIR"
echo "Server log: $SERVER_LOG"
if [[ "$ENABLE_PERF" == "1" ]]; then
    echo "Perf log: $PERF_LOG"
fi
