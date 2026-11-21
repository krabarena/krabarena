#!/usr/bin/env bash
# scripts/load-test.sh
#
# Measure page-render performance for the three public-facing
# endpoints in both HTML and markdown variants. Useful as:
#
#   - a perf regression guard before merging a PR that touches
#     anything in the home/battle/claim render path
#   - a baseline harness when investigating slow pages
#   - a quick sanity check the SQL doesn't N+1 a freshly forked
#     dev branch
#
# WHAT IT MEASURES
#   For each (endpoint × variant), runs N requests sequentially
#   (default 10), captures total wall-clock time via curl's
#   `%{time_total}`, and prints min / p50 / p90 / p99 / max plus
#   response size. Discards the first request as a warm-up so the
#   reported numbers reflect a warm Neon pool + warm CMS cache.
#
# WHAT IT DOESN'T MEASURE (yet)
#   Per-request SQL query count. We don't have request-scoped
#   counters in the server. Workaround: run the server with
#   `RUST_LOG=sqlx::query=debug` and grep `/tmp/server.log` for
#   `query:` lines while a single request runs, or instrument the
#   pool with a tower middleware. Not in this script — too noisy
#   on Cloud Run logs to be the default.
#
# USAGE
#   scripts/load-test.sh [--api-url URL] [-n REQUESTS]
#
# DEFAULTS
#   --api-url   http://127.0.0.1:3001
#   -n          10
#
# EXAMPLES
#   ./scripts/load-test.sh                              # local
#   ./scripts/load-test.sh --api-url $TEST_URL          # test env
#   ./scripts/load-test.sh -n 30                        # 30 hits per endpoint

set -euo pipefail

API_URL="http://127.0.0.1:3001"
N=10

while [ $# -gt 0 ]; do
  case "$1" in
    --api-url) API_URL="$2"; shift 2 ;;
    -n)        N="$2";       shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Auto-discover a real battle slug + a real claim slug for the
# fixture set. The home page surfaces top claims platform-wide
# (top_claims block), so we look there first — falling back to
# scraping the first battle's page only if the home page has no
# claim links (fresh fork).
# Pipe pattern: `grep | head -1` under pipefail can trip when head
# closes the pipe early — `|| true` keeps us going if the page
# response is fine but extraction produced nothing.
echo "==> Discovering fixture URLs from $API_URL ..."
HOME_HTML=$(curl -fsS -H 'Accept: text/html' "$API_URL/")
SLUG=$(echo "$HOME_HTML" | grep -oE '/battles/[a-z0-9-]+' | head -1 || true)
[ -z "$SLUG" ] && { echo "error: no battles found at $API_URL" >&2; exit 1; }

CLAIM=$(echo "$HOME_HTML" | grep -oE '/claims/[a-z0-9-]{20,}' | head -1 || true)
if [ -z "$CLAIM" ]; then
  BATTLE_HTML=$(curl -fsS -H 'Accept: text/html' "$API_URL$SLUG")
  CLAIM=$(echo "$BATTLE_HTML" | grep -oE '/claims/[a-z0-9-]{20,}' | head -1 || true)
fi

echo "    battle: $SLUG"
echo "    claim:  ${CLAIM:-<none — claim test skipped>}"

# OG-card render path (since 0.28.6 it runs the full `build_wins_matrix`
# standings on every render — a perf-sensitive spot the page bench below
# doesn't otherwise cover). Discover a battle UUID for the /og-html hit.
OG_BID=$(curl -fsS "$API_URL/v1/battles?limit=1" 2>/dev/null \
          | grep -oE '"id":"[0-9a-f-]{36}"' | head -1 \
          | sed 's/.*"id":"//; s/"//' || true)
echo "    og bid: ${OG_BID:-<none — og test skipped>}"
echo

# Stats helper: stdin = newline-separated seconds. Sorts externally
# (BSD awk has no `asort`), then reads each value in ascending order
# and computes percentiles. Prints count + min/p50/p90/p99/max/mean.
stats() {
  sort -n | awk -v label="$1" '
    { n++; v[n] = $1; sum += $1 }
    END {
      p50 = v[int(n*0.5+0.5)]
      p90 = v[int(n*0.9+0.5)]
      p99 = v[int(n*0.99+0.5)]
      printf "  %-50s  n=%2d  min=%6.3fs  p50=%6.3fs  p90=%6.3fs  p99=%6.3fs  max=%6.3fs  mean=%6.3fs\n",
             label, n, v[1], p50, p90, p99, v[n], sum/n
    }'
}

# Run one (path, accept, label) bench: warm up, hit N times, pipe to stats.
# Also peeks at the `X-DB-Queries` response header from one warm hit so the
# output shows per-request query count next to the latency stats.
bench_one() {
  local path="$1" accept="$2" label="$3"
  # Warmup (excluded from stats — pool + CMS cache + ServeDir etc.)
  curl -s -L -o /dev/null -H "Accept: $accept" "$API_URL$path"
  # Pull the X-DB-Queries header from a fresh request (-I HEAD).
  # `-L` follows redirects; the count for the final response is the last header line.
  local qcount
  qcount=$(curl -s -L -I -H "Accept: $accept" "$API_URL$path" \
            | grep -i '^x-db-queries:' \
            | tail -1 | awk '{print $2}' | tr -d '\r' || true)
  qcount=${qcount:-?}
  # Capture timings
  local timings
  timings=$(for _ in $(seq 1 "$N"); do
    curl -s -L -o /dev/null -H "Accept: $accept" -w "%{time_total}\n" "$API_URL$path"
  done)
  echo "$timings" | stats "$label  (queries=$qcount)"
}

# Build the test grid.
echo "==> Bench plan (N=$N each, +1 warmup discarded)"
echo
declare -a TESTS=(
  "/                                  text/html       /  (html)"
  "/                                  text/markdown   /  (md) "
  "$SLUG                              text/html       /battles/{slug}  (html)"
  "$SLUG                              text/markdown   /battles/{slug}  (md) "
)
[ -n "$CLAIM" ] && TESTS+=(
  "$CLAIM                             text/html       /claims/{slug}   (html)"
  "$CLAIM                             text/markdown   /claims/{slug}   (md) "
)
TESTS+=(
  "/agents                            text/html       /agents  (html)"
  "/agents                            text/markdown   /agents  (md) "
  "/arena                             text/html       /arena   (html)"
  "/arena                             text/markdown   /arena   (md) "
)

START=$(date +%s)
for row in "${TESTS[@]}"; do
  # crude column split — works because path / accept never contain whitespace
  PATH_FOR_HIT=$(echo "$row" | awk '{print $1}')
  ACCEPT=$(echo "$row" | awk '{print $2}')
  LABEL=$(echo "$row" | awk '{ $1=""; $2=""; print }' | sed 's/^[[:space:]]*//')
  bench_one "$PATH_FOR_HIT" "$ACCEPT" "$LABEL"
done
DUR=$(( $(date +%s) - START ))

echo
echo "==> Done in ${DUR}s wall-clock"
echo

# ---- Latency budget gate (regression guard) --------------------------
# Re-samples the key render paths (incl. the OG card, which runs the full
# build_wins_matrix since 0.28.6) and checks p95 against a budget. Exits
# non-zero if any endpoint is over — so this script doubles as a CI/pre-
# deploy guard, not just an exploratory print. Budgets are generous (ad-
# hoc cross-region noise from a laptop) but catch a gross regression (an
# N+1, a missing index, a runaway in-memory build). Override per-endpoint:
#   BUDGET_HOME / BUDGET_BATTLE / BUDGET_CLAIM / BUDGET_OG  (seconds).
echo "==> Latency budget gate (p95 — fails the run on a regression)"
FAILED=0
gate() {
  local path="$1" accept="$2" budget="$3" label="$4"
  [ -z "$path" ] && { printf "  %-26s  SKIP (no fixture)\n" "$label"; return; }
  curl -s -L -o /dev/null -H "Accept: $accept" "$API_URL$path"   # warmup
  local p95
  p95=$(for _ in $(seq 1 "$N"); do
          curl -s -L -o /dev/null -H "Accept: $accept" -w "%{time_total}\n" "$API_URL$path"
        done | sort -n | awk '{n++;v[n]=$1} END{print v[int(n*0.95+0.5)]}')
  local verdict="PASS"
  if ! awk "BEGIN{exit !($p95 <= $budget)}"; then verdict="FAIL"; FAILED=$((FAILED+1)); fi
  printf "  %-26s  p95=%6.3fs  budget=%5.2fs  [%s]\n" "$label" "$p95" "$budget" "$verdict"
}
gate "/"                          text/html "${BUDGET_HOME:-2.0}"   "/ (html)"
gate "$SLUG"                      text/html "${BUDGET_BATTLE:-1.2}" "/battles/{slug}"
gate "$CLAIM"                     text/html "${BUDGET_CLAIM:-1.2}"  "/claims/{slug}"
[ -n "${OG_BID:-}" ] && gate "/og-html/battles/$OG_BID" text/html "${BUDGET_OG:-1.5}" "/og-html/battles/{id}"
echo
if [ "$FAILED" -gt 0 ]; then
  echo "==> BUDGET GATE: $FAILED endpoint(s) OVER budget — possible latency regression"
else
  echo "==> BUDGET GATE: all within budget — no latency degradation"
fi

cat <<'NOTES'

Per-render DB call count (from source — `crates/krabarena-api/src/web/pages.rs`):

| Endpoint          | DB queries per render              | Notes                                                              |
|-------------------|------------------------------------|--------------------------------------------------------------------|
| /  (html or md)   | 8 base                             | battles list, cost-aggregator, 3× COUNT, top_claims, activity,     |
|                   |   + 3 CMS reads (cold cache)       | top_agents. CMS reads cache in-process for 60s. md slices the     |
|                   |                                    | first 5 cards from the same `battles` Vec — no extra SQL.          |
| /battles/{slug}   | 7 base                             | battle by slug, stats COUNT, claims, sponsor lookup,               |
|                   |   + 2 CMS reads (cold cache)       | fetch_battle_events (×2), battle_cost_aggregates.                  |
| /claims/{slug}    | 7 base                             | claim by slug, author display name, verifications, comments,       |
|                   |   + 2 CMS reads (cold cache)       | user_names batch, load_review_cost, battle lookup for crumb.       |

Cold-cache CMS overhead lasts 60 s after each process start; after that
the in-process cache (`Mutex<HashMap>`) absorbs every read. To see live
SQL during a hit, run the server with:

  RUST_LOG=sqlx=info,sqlx::query=info  cargo run -p krabarena-api

and grep your server log for the issued SQL while a single request lands
against it.

Latency dominance check: each query through Neon's pooler is bounded by
TCP RTT from your machine to us-east-2 (~30-300 ms depending on origin).
8 sequential queries ⇒ ~250 ms - 2.4 s floor on `/` even with a hot DB.
Same-region (Cloud Run in us-central, Neon in us-east) sees ~30 ms each
≈ 240 ms floor. If your local p50 is far above 300 ms × N_queries,
something else is happening (cold compute, big payload, network).
NOTES

# Non-zero exit when any endpoint blew its p95 budget — lets CI / a
# pre-deploy step use `scripts/load-test.sh --api-url $URL` as a gate.
exit "${FAILED:-0}"
