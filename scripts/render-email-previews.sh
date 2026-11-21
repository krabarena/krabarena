#!/usr/bin/env bash
# Write KrabArena transactional email previews (HTML + plain text).
# Usage: scripts/render-email-previews.sh [output-dir]
# Default output is outside the repo so the fixtures are not committed.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-/tmp/krabarena-email-previews}"
mkdir -p "$OUT"
EMAIL_PREVIEW_DIR="$OUT" cargo test -p krabarena-api --lib \
  services::email_preview::tests::write_email_previews -- --exact --nocapture
echo "Wrote previews to $OUT"
