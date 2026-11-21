#!/usr/bin/env bash
# Run the krabarena-api E2E tests locally, against a dedicated Neon test
# branch — never against prod.
#
# Usage:
#   scripts/run-e2e.sh
#
# Environment (either set in your shell or in .env.test.local):
#   TEST_DATABASE_URL   Neon connection string for the test branch. If
#                       unset, we try `neonctl connection-string test-e2e`.
#   GEMINI_API_KEY      Required — real key so the KrabReviewer runs.
#   JWT_SECRET          Any string; must be internally consistent for the
#                       duration of the run (defaults to a per-run random).
#   PORT                Local port for the server (default: 3100).
#
# The script:
#   1. Resolves env (file + shell + neonctl fallback).
#   2. Generates a fresh TEST_AUTH_SECRET for this run only.
#   3. Starts krabarena-api pointed at the test branch.
#   4. Waits for /health.
#   5. Runs `cargo test -p krabarena-api --test review_e2e -- --ignored`.
#   6. Kills the server and exits with the test result.
#
# To reset accumulated rows in the test branch between runs:
#   neonctl branches delete test-e2e --project-id "$NEON_PROJECT_ID"
#   neonctl branches create --name test-e2e --parent main \
#     --project-id "$NEON_PROJECT_ID"

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ENV_FILE="$REPO_ROOT/.env.test.local"
if [[ -f "$ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    set -a; source "$ENV_FILE"; set +a
fi

# Resolve TEST_DATABASE_URL — env wins, else neonctl.
if [[ -z "${TEST_DATABASE_URL:-}" ]]; then
    if command -v neonctl >/dev/null 2>&1; then
        echo "TEST_DATABASE_URL unset — resolving via neonctl..."
        TEST_DATABASE_URL="$(neonctl connection-string test-e2e \
            --project-id "${NEON_PROJECT_ID:-}" 2>/dev/null || true)"
    fi
fi
if [[ -z "${TEST_DATABASE_URL:-}" ]]; then
    echo "ERROR: TEST_DATABASE_URL not set and neonctl lookup failed." >&2
    echo "Put it in .env.test.local or export it in your shell." >&2
    exit 1
fi

# Refuse to run against prod-looking URLs.
if [[ "$TEST_DATABASE_URL" == *"ep-fragrant-dawn"* ]]; then
    echo "ERROR: TEST_DATABASE_URL points at the prod Neon endpoint." >&2
    echo "Use the test-e2e branch instead." >&2
    exit 1
fi

if [[ -z "${GEMINI_API_KEY:-}" ]]; then
    echo "ERROR: GEMINI_API_KEY is required so the KrabReviewer runs." >&2
    echo "(Pull it from Cloud Run env if you have gcloud access.)" >&2
    exit 1
fi

export DATABASE_URL="$TEST_DATABASE_URL"
export JWT_SECRET="${JWT_SECRET:-$(openssl rand -hex 32)}"
export BLOB_STORE_URL="${BLOB_STORE_URL:-gs://krabarena-artifacts}"
export GEMINI_API_KEY
export TEST_AUTH_SECRET="$(openssl rand -hex 32)"
PORT="${PORT:-3100}"
export LISTEN_ADDR="127.0.0.1:${PORT}"

echo "building krabarena-api..."
cargo build -p krabarena-api --quiet

LOG="$(mktemp -t krab-server.XXXXXX.log)"
echo "starting server on ${LISTEN_ADDR} (log: $LOG)"
./target/debug/krabarena-api > "$LOG" 2>&1 &
SERVER_PID=$!
cleanup() {
    if kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# Wait for /health (up to ~20s for cold migrations).
for _ in $(seq 1 40); do
    if curl -sf "http://127.0.0.1:${PORT}/health" >/dev/null; then
        break
    fi
    sleep 0.5
done
if ! curl -sf "http://127.0.0.1:${PORT}/health" >/dev/null; then
    echo "ERROR: server didn't come up. Log tail:" >&2
    tail -40 "$LOG" >&2
    exit 1
fi
echo "server healthy."

echo "running E2E tests..."
TEST_AUTH_SECRET="$TEST_AUTH_SECRET" \
KRAB_TEST_BASE="http://127.0.0.1:${PORT}" \
    cargo test -p krabarena-api --test review_e2e -- --ignored --nocapture
