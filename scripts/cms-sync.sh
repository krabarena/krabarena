#!/usr/bin/env bash
# scripts/cms-sync.sh
#
# Push the canonical CMS-block markdown files
# (`crates/krabarena-api/src/services/cms_defaults/*.md`) to a running
# server's `config` table via the admin endpoint
# (`PUT /v1/admin/config/{key}`).
#
# Use case: a new branch updates the default copy in the `.md`
# files. Rebuild the server and merge — but existing prod DB rows
# (seeded by migration 025 long ago) still hold the OLD text and
# the in-process service reads them first. This script refreshes
# the live DB rows from the source files. Cache invalidation
# happens write-through on each PUT so the new copy shows up on
# the very next render.
#
# USAGE
#   scripts/cms-sync.sh [--api-url URL] [--token TOK]
#
# DEFAULTS
#   --api-url  http://127.0.0.1:3001
#   --token    minted from POST /v1/test/mint-token (requires
#              TEST_AUTH_SECRET=letmein on the server). For prod
#              you must pass --token <admin-jwt>.
#
# EXAMPLES
#   # Local dev (TEST_AUTH_SECRET=letmein on the server):
#   ./scripts/cms-sync.sh
#
#   # Test env on Cloud Run:
#   ./scripts/cms-sync.sh \
#     --api-url "$TEST_PUBLIC_BASE_URL" \
#     --token "$(curl -s -X POST -H 'x-test-auth: '$TEST_AUTH_SECRET \
#                 -H 'content-type: application/json' \
#                 -d '{"user":"test-admin"}' \
#                 "$URL/v1/test/mint-token" | jq -r '.data.bearer_token')"
#
#   # Prod (paste your own admin JWT — there's no test-mint there):
#   ./scripts/cms-sync.sh --api-url https://krabarena.org --token "$ADMIN_JWT"

set -euo pipefail

API_URL="http://127.0.0.1:3001"
TOKEN=""

while [ $# -gt 0 ]; do
  case "$1" in
    --api-url) API_URL="$2"; shift 2 ;;
    --token)   TOKEN="$2";   shift 2 ;;
    -h|--help)
      sed -n '2,/^set -euo/p' "$0" | sed 's/^# \?//'; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Resolve the defaults dir relative to this script so it works from any cwd.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DEFAULTS_DIR="${SCRIPT_DIR}/../crates/krabarena-api/src/services/cms_defaults"
if [ ! -d "$DEFAULTS_DIR" ]; then
  echo "error: defaults dir not found: $DEFAULTS_DIR" >&2
  exit 1
fi

# Mint a token via the test endpoint if none was provided (local only).
if [ -z "$TOKEN" ]; then
  echo "==> No --token; minting test-admin via $API_URL/v1/test/mint-token"
  TOKEN=$(curl -fsS -X POST \
    -H 'x-test-auth: letmein' \
    -H 'content-type: application/json' \
    -d '{"user":"test-admin"}' \
    "$API_URL/v1/test/mint-token" | jq -r '.data.bearer_token')
  if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    echo "error: mint-token failed (set TEST_AUTH_SECRET=letmein on the server, or pass --token)" >&2
    exit 1
  fi
fi

# (cms_key, source_file) pairs — keep aligned with cms_service.rs constants.
KEY_FILE_PAIRS=(
  "cms_intro       intro.md"
  "cms_install     install.md"
  "cms_next_home   next_home.md"
  "cms_next_battle next_battle.md"
  "cms_next_claim  next_claim.md"
)

for pair in "${KEY_FILE_PAIRS[@]}"; do
  KEY=$(echo "$pair" | awk '{print $1}')
  FILE=$(echo "$pair" | awk '{print $2}')
  FULL_PATH="${DEFAULTS_DIR}/${FILE}"

  if [ ! -f "$FULL_PATH" ]; then
    echo "error: source file missing: $FULL_PATH" >&2
    exit 1
  fi

  # The admin endpoint accepts {"value": "..."} — jq's `-Rs` reads
  # the whole file as one string, encoded as JSON safely (handles
  # newlines, backslashes, quotes correctly).
  PAYLOAD=$(jq -Rs '{value: .}' < "$FULL_PATH")

  echo "==> PUT $KEY  ($(wc -c < "$FULL_PATH") bytes)"
  RESP=$(curl -fsS -X PUT \
    -H "Authorization: Bearer $TOKEN" \
    -H 'content-type: application/json' \
    -d "$PAYLOAD" \
    "$API_URL/v1/admin/config/$KEY")
  echo "    $(echo "$RESP" | jq -c '.data')"
done

echo
echo "✓ Synced ${#KEY_FILE_PAIRS[@]} CMS blocks to $API_URL"
echo "  Verify: curl $API_URL/ | head -20"
