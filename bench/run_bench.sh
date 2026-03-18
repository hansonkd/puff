#!/bin/bash
set -e

# Puff Free-Threaded Python Benchmark
# Compares GIL Python vs free-threaded Python under concurrent load

PUFF_BIN="$(cd "$(dirname "$0")/.." && cargo build --release 2>&1 | tail -1 && echo "target/release/puff")"
PUFF_BIN="$(cd "$(dirname "$0")/.." && echo "target/release/puff")"
BENCH_DIR="$(cd "$(dirname "$0")" && pwd)"

PORT=7799
WRK_THREADS=4
WRK_CONNECTIONS=100
WRK_DURATION=10s
ENDPOINT="/cpu"

echo "============================================="
echo "  Puff Free-Threaded Python Benchmark"
echo "============================================="
echo ""
echo "Endpoint:     $ENDPOINT"
echo "Connections:  $WRK_CONNECTIONS"
echo "Duration:     $WRK_DURATION"
echo "wrk threads:  $WRK_THREADS"
echo ""

# Build puff
echo "Building Puff (release)..."
cd "$(dirname "$0")/.."
cargo build --release 2>&1 | tail -3
cd "$BENCH_DIR"

echo ""
echo "---------------------------------------------"
echo "  Test 1: Python 3.12 (GIL)"
echo "---------------------------------------------"

# Start puff with system Python
PUFF_CONFIG="$BENCH_DIR/puff_bench.toml" ../target/release/puff serve &
PID=$!
sleep 2

# Verify it's running
if ! kill -0 $PID 2>/dev/null; then
    echo "ERROR: Puff failed to start with Python 3.12"
    exit 1
fi

# Warmup
wrk -t1 -c10 -d2s "http://localhost:$PORT$ENDPOINT" > /dev/null 2>&1

# Benchmark
echo "Running wrk..."
wrk -t$WRK_THREADS -c$WRK_CONNECTIONS -d$WRK_DURATION --latency "http://localhost:$PORT$ENDPOINT" 2>&1

kill $PID 2>/dev/null
wait $PID 2>/dev/null || true
sleep 1

echo ""
echo "---------------------------------------------"
echo "  Test 2: Python 3.13t (Free-Threaded)"
echo "---------------------------------------------"

# Start puff with free-threaded Python
# PyO3 uses the Python found in PATH or VIRTUAL_ENV
# We need to point it at our 3.13t build
PYO3_PYTHON=/opt/python3.13t/bin/python3.13t PUFF_CONFIG="$BENCH_DIR/puff_bench.toml" ../target/release/puff serve &
PID=$!
sleep 2

if ! kill -0 $PID 2>/dev/null; then
    echo "ERROR: Puff failed to start with Python 3.13t"
    echo "NOTE: PyO3 may need to be recompiled for 3.13t"
    exit 1
fi

# Warmup
wrk -t1 -c10 -d2s "http://localhost:$PORT$ENDPOINT" > /dev/null 2>&1

# Benchmark
echo "Running wrk..."
wrk -t$WRK_THREADS -c$WRK_CONNECTIONS -d$WRK_DURATION --latency "http://localhost:$PORT$ENDPOINT" 2>&1

kill $PID 2>/dev/null
wait $PID 2>/dev/null || true

echo ""
echo "============================================="
echo "  Benchmark complete"
echo "============================================="
echo ""
echo "Key metrics to compare:"
echo "  - Requests/sec (higher = better)"
echo "  - Latency avg/p99 (lower = better)"
echo ""
echo "Expected: Free-threaded should show significantly"
echo "higher throughput on the /cpu endpoint since Python"
echo "threads can truly run in parallel."
