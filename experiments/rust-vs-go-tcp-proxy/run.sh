#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

PIDS=()
cleanup() { for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null; }
trap cleanup EXIT

echo "=== Building Rust proxy ==="
(cd rust-proxy && cargo build --release 2>&1 | tail -1)

echo "=== Building Go proxy ==="
(cd go-proxy && go build -o go-proxy . 2>&1)

echo "=== Starting echo backend ==="
python3 echo_server.py &
PIDS+=($!)
sleep 1

echo "=== Benchmarking Rust proxy (port 8001) ==="
./rust-proxy/target/release/tcp-proxy &
PIDS+=($!)
sleep 1
RUST_START=$(python3 -c "import time; print(int(time.time()*1000))")
RUST_RESULT=$(python3 bench.py 8001)
RUST_END=$(python3 -c "import time; print(int(time.time()*1000))")
RUST_DURATION=$((RUST_END - RUST_START))
kill "${PIDS[-1]}" 2>/dev/null || true; wait "${PIDS[-1]}" 2>/dev/null || true
unset 'PIDS[-1]'
echo "Rust: $RUST_RESULT (${RUST_DURATION}ms)"
sleep 1

echo "=== Benchmarking Go proxy (port 8002) ==="
./go-proxy/go-proxy &
PIDS+=($!)
sleep 1
GO_START=$(python3 -c "import time; print(int(time.time()*1000))")
GO_RESULT=$(python3 bench.py 8002)
GO_END=$(python3 -c "import time; print(int(time.time()*1000))")
GO_DURATION=$((GO_END - GO_START))
kill "${PIDS[-1]}" 2>/dev/null || true; wait "${PIDS[-1]}" 2>/dev/null || true
unset 'PIDS[-1]'
echo "Go:   $GO_RESULT (${GO_DURATION}ms)"

# Merge results
python3 -c "
import json, platform
rust = json.loads('$RUST_RESULT')
go = json.loads('$GO_RESULT')
out = {
    'rust': rust,
    'go': go,
    'metadata': {
        'concurrency': 500,
        'iterations_per_conn': 100,
        'payload_bytes': 64,
        'platform': platform.platform(),
        'cpu': platform.processor() or platform.machine(),
        'rust_duration_ms': $RUST_DURATION,
        'go_duration_ms': $GO_DURATION,
    }
}
with open('results.json', 'w') as f:
    json.dump(out, f, indent=2)
print()
print('=== Results ===')
print(f\"{'Metric':<12} {'Rust (us)':>12} {'Go (us)':>12} {'Ratio (Go/Rust)':>16}\")
print('-' * 56)
for key in ['p50_us', 'p90_us', 'p99_us', 'p99_9_us']:
    r = rust[key]; g = go[key]
    ratio = g / r if r > 0 else 0
    label = key.replace('_us','').replace('_','.').upper()
    print(f'{label:<12} {r:>12.1f} {g:>12.1f} {ratio:>15.2f}x')
print(f\"{'Requests':<12} {rust['total_requests']:>12} {go['total_requests']:>12}\")
print(f\"{'Errors':<12} {rust['errors']:>12} {go['errors']:>12}\")
"

echo ""
echo "Results saved to results.json"
