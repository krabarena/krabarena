#!/usr/bin/env bash
# local-test.sh — build the og-render image, run it, and probe it.
#
# Per AGENTS.md "local Docker test before GitHub push": this service
# ships a browser, so build + run + curl it locally before pushing.
# The probe renders a throwaway static HTML page (no krabarena-api
# needed) and asserts a valid PNG comes back from /render, plus /health.
#
# Usage:
#   ./local-test.sh            # build + run + probe + clean up
#   ./local-test.sh build      # build image only
#   ./local-test.sh probe      # assume the image exists; run + probe
#
set -euo pipefail
cd "$(dirname "$0")"

IMG="og-render-local:test"
CTR="og-render-local"
PORT="${PORT:-8099}"

build() {
  echo "==> docker build $IMG"
  docker build -t "$IMG" .
}

cleanup() {
  docker rm -f "$CTR" >/dev/null 2>&1 || true
  rm -f /tmp/og-render-probe.html /tmp/og-render-probe.png
}

probe() {
  cleanup
  trap cleanup EXIT

  # Throwaway static page served from inside the container via a
  # file:// URL — exercises the full Chromium screenshot path without
  # needing the krabarena-api running. 1200x630, the OG card size.
  cat > /tmp/og-render-probe.html <<'HTML'
<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;width:1200px;height:630px}
body{background:#faf6ee;color:#1a1a18;font-family:sans-serif;
display:flex;align-items:center;justify-content:center;font-size:64px;font-weight:900}
</style></head><body>🦀 OG RENDER PROBE</body></html>
HTML

  echo "==> docker run $CTR on :$PORT"
  docker run -d --name "$CTR" -p "$PORT:8080" \
    -e OG_RENDER_ALLOW_FILE_URLS=1 \
    -v /tmp/og-render-probe.html:/probe/page.html:ro \
    "$IMG" >/dev/null

  echo "==> waiting for /health"
  for i in $(seq 1 30); do
    if curl -fsS "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
      break
    fi
    sleep 1
    if [ "$i" -eq 30 ]; then
      echo "FAIL: /health never came up"; docker logs "$CTR"; exit 1
    fi
  done
  echo "--- /health:"
  curl -fsS "http://127.0.0.1:$PORT/health"; echo

  echo "==> rendering the throwaway page via /render?url=file://…"
  curl -fsS "http://127.0.0.1:$PORT/render?url=file:///probe/page.html" \
    -o /tmp/og-render-probe.png

  # Validate PNG magic bytes + non-trivial size.
  MAGIC=$(head -c 8 /tmp/og-render-probe.png | xxd -p)
  SIZE=$(wc -c < /tmp/og-render-probe.png | tr -d ' ')
  echo "--- rendered PNG: $SIZE bytes, magic=$MAGIC"
  if [ "$MAGIC" != "89504e470d0a1a0a" ]; then
    echo "FAIL: body is not a PNG"; docker logs "$CTR"; exit 1
  fi
  if [ "$SIZE" -lt 1000 ]; then
    echo "FAIL: PNG suspiciously small ($SIZE bytes)"; exit 1
  fi
  echo "PASS: og-render returned a valid $SIZE-byte PNG"
}

case "${1:-all}" in
  build) build ;;
  probe) probe ;;
  all) build; probe ;;
  *) echo "usage: $0 [build|probe|all]"; exit 2 ;;
esac
