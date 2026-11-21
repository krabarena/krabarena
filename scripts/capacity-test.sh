#!/usr/bin/env bash
# scripts/capacity-test.sh
#
# Find the max sustainable RPS for the public-facing pages and the
# concurrency level where it stops scaling ("the knee"). Companion
# to `scripts/load-test.sh`, which measures per-request latency in
# isolation — this one hits the same endpoints concurrently.
#
# Tool: `wrk` (Homebrew: `brew install wrk`). Picked over `hey`/`k6`
# because (a) it's already on most dev boxes, (b) one binary, no deps,
# and (c) it prints requests/sec + latency histogram + socket-error
# counts in one shot — exactly what we want per concurrency step.
#
# WHAT IT MEASURES
#   For each (endpoint, concurrency) pair: 20s of sustained traffic,
#   then prints requests/sec, p50/p90/p99 latency, socket errors,
#   non-2xx responses. The runner ramps concurrency through a
#   configurable schedule (default: 1, 5, 10, 20, 40, 80). The knee
#   is where RPS stops growing or p99 hockey-sticks — usually around
#   the DB pool ceiling on a debug build.
#
# CAVEATS
#   - Server is built `--debug` in dev; for an honest capacity number,
#     run `cargo build --release -p krabarena-api` first.
#   - Pool size is currently 20 (`crates/krabarena-api/src/db/mod.rs`).
#     Concurrency past 20 measures min(pool, app), not pure code
#     capacity. Bump max_connections to push the test further.
#   - Neon test-e2e lives on shared compute and auto-suspends after
#     ~5 min idle; the first warmup hit may pay the cold-start.
#   - Local-to-Neon RTT (us-east-2) is ~30-300ms depending on origin.
#     Same-region Cloud Run sees ~30ms each. A 6-query endpoint floors
#     at 6×RTT regardless of how fast the app is.
#
# USAGE
#   scripts/capacity-test.sh [--api-url URL] [-d 20s]
#                            [--concurrency "1 5 10 20 40 80"]
#                            [--endpoints "/ /agents /arena"]
#
# DEFAULTS
#   --api-url       http://127.0.0.1:3001
#   -d              20s
#   --concurrency   "1 5 10 20 40 80"
#   --endpoints     auto-discovered (/, /agents, /arena, first battle)
#
# EXAMPLES
#   scripts/capacity-test.sh                      # local, default ramp
#   scripts/capacity-test.sh -d 10s               # shorter (~6 min total)
#   scripts/capacity-test.sh --concurrency "20 40 80 160"  # high end only
#   scripts/capacity-test.sh --api-url $TEST_URL  # test env

set -euo pipefail

API_URL="http://127.0.0.1:3001"
DURATION="20s"
CONCURRENCY="1 5 10 20 40 80"
ENDPOINTS_OVERRIDE=""

while [ $# -gt 0 ]; do
  case "$1" in
    --api-url)     API_URL="$2"; shift 2 ;;
    -d)            DURATION="$2"; shift 2 ;;
    --concurrency) CONCURRENCY="$2"; shift 2 ;;
    --endpoints)   ENDPOINTS_OVERRIDE="$2"; shift 2 ;;
    -h|--help)     sed -n '2,/^set -euo/p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

command -v wrk >/dev/null 2>&1 || {
  echo "error: wrk not found. Install: brew install wrk" >&2
  exit 1
}

# Auto-discover a real battle slug for the default endpoint set, so
# the runner stays honest against a fresh fork. Caller can override
# with --endpoints to point at exactly the paths they care about.
if [ -z "$ENDPOINTS_OVERRIDE" ]; then
  echo "==> Discovering battle slug from $API_URL ..."
  HOME_HTML=$(curl -fsS -H 'Accept: text/html' "$API_URL/")
  SLUG=$(echo "$HOME_HTML" | grep -oE '/battles/[a-z0-9-]+' | head -1 || true)
  [ -z "$SLUG" ] && { echo "error: no battles at $API_URL" >&2; exit 1; }
  ENDPOINTS="/agents /arena / $SLUG"
  echo "    using: $ENDPOINTS"
  echo
else
  ENDPOINTS="$ENDPOINTS_OVERRIDE"
fi

# Warm up the Neon compute + CMS cache so step c=1 doesn't pay the
# cold-start. One pass through all endpoints at c=1, 3s — discarded.
echo "==> Warmup pass (Neon compute + CMS cache, 3s each, discarded)"
for path in $ENDPOINTS; do
  wrk -t 1 -c 1 -d 3s "$API_URL$path" >/dev/null 2>&1 || true
done
echo

# Parse `wrk` output for the numbers we care about. The text format
# is stable across wrk versions but irritating to scrape; this awk
# pulls latency p50/p90/p99 from the `Latency Distribution` block
# (only present when -L / --latency is on, but we always pass it),
# Requests/sec from the final summary, socket errors and non-2xx
# from the error tally.
parse_wrk() {
  awk '
    /Requests\/sec:/ { rps = $2 }
    / 50%/  { p50 = $2 }
    / 90%/  { p90 = $2 }
    / 99%/  { p99 = $2 }
    /Socket errors:/ { errs = $0; sub(/^.*Socket errors: */, "", errs) }
    /Non-2xx or 3xx responses:/ { non2xx = $4 }
    END {
      if (!rps)     rps = "0"
      if (!p50)     p50 = "?"
      if (!p90)     p90 = "?"
      if (!p99)     p99 = "?"
      if (!errs)    errs = "0"
      if (!non2xx)  non2xx = "0"
      printf "%s|%s|%s|%s|%s|%s\n", rps, p50, p90, p99, errs, non2xx
    }
  '
}

# Pretty-printer for the per-endpoint table. Six columns: concurrency,
# RPS, p50, p90, p99, errors (socket + non-2xx, merged). Right-aligned
# numerics so it eyeballs cleanly even when scaled into thousands.
print_header() {
  printf "  %-4s %12s %10s %10s %10s %20s\n" "c" "rps" "p50" "p90" "p99" "errors"
}

# Run one endpoint × concurrency sweep, print the row table.
sweep_endpoint() {
  local path="$1"
  echo "==> $path"
  print_header
  for c in $CONCURRENCY; do
    # `-t` (threads): up to half the connections, max #CPUs. Keeps
    # wrk from over-subscribing the host. `-L` enables latency stats.
    local t
    t=$(( c < 2 ? 1 : c / 2 ))
    local cpus
    cpus=$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo 4)
    [ "$t" -gt "$cpus" ] && t="$cpus"
    local out
    out=$(wrk -t "$t" -c "$c" -d "$DURATION" -L "$API_URL$path" 2>&1)
    local parsed
    parsed=$(echo "$out" | parse_wrk)
    local rps p50 p90 p99 errs non2xx
    IFS='|' read -r rps p50 p90 p99 errs non2xx <<< "$parsed"
    # Combine socket errors + non-2xx into one display cell so the
    # row stays narrow. `errs` from wrk is itself multi-field text
    # (e.g. "connect 0, read 0, write 0, timeout 0") — that's
    # legible enough in the table even when it wraps slightly.
    local err_combined="$errs"
    if [ "$non2xx" != "0" ]; then
      err_combined="$err_combined non2xx=$non2xx"
    fi
    printf "  %-4s %12s %10s %10s %10s %20s\n" \
      "$c" "$rps" "$p50" "$p90" "$p99" "$err_combined"
  done
  echo
}

echo "==> Capacity sweep against $API_URL (duration=$DURATION per step)"
echo "==> Concurrency steps: $CONCURRENCY"
echo

START=$(date +%s)
for path in $ENDPOINTS; do
  sweep_endpoint "$path"
done
DUR=$(( $(date +%s) - START ))

echo "==> Done in ${DUR}s wall-clock"
echo

cat <<'NOTES'
How to read this:

  - RPS column should grow with concurrency until it plateaus —
    that's "the knee". Anything past the knee is just longer queue
    waits, not more throughput.
  - p99 jumping by 10× without RPS growing = saturated. Usually the
    DB pool (max_connections=20) at high concurrency on the local
    debug build.
  - Non-zero `errors` at low concurrency = real bug. At high
    concurrency under saturation, transient socket timeouts are
    normal — the server isn't dying, it's just queueing.

To push the test further:
  1. cargo build --release -p krabarena-api      # ~2-3× faster
  2. bump max_connections in src/db/mod.rs
  3. run against same-region infra (Cloud Run hitting Neon in
     us-east-2) — local-to-Neon RTT dominates the per-request
     floor and masks app-level capacity.
NOTES
