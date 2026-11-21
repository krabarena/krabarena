# og-render — KrabArena OG-card render microservice

Screenshots the krabarena-api `/og-html/...` leaderboard templates into
1200×630 PNG social cards using a single, warm headless Chromium
(Playwright).

This service exists so **Chromium stays out of the main Rust API
container**. The API renders an HTML template (`/og-html/battles/{id}`,
`/og-html/claims/{id}`); this service navigates to it, screenshots it,
and returns the PNG. The API's `/og/{kind}/{id}` route calls here when
`OG_RENDER_URL` is set, caches the result in GCS, and falls back to its
built-in `tiny_skia` card on any failure.

## Routes

| Route | Purpose |
| --- | --- |
| `GET /health` | Liveness — `{status, browser, html_base}`. |
| `GET /render?path=/og-html/battles/<id>` | Navigate to `KRAB_HTML_BASE + path`, screenshot, return `image/png`. |
| `GET /render?url=https://…` | Navigate to a full absolute URL instead (used by the local smoke test). |

## Environment

| Var | Default | Notes |
| --- | --- | --- |
| `PORT` | `8080` | Listen port. |
| `KRAB_HTML_BASE` | — | Base URL of the krabarena-api serving `/og-html` (e.g. `https://krabarena.com`). Required for the `path=` form. |
| `RENDER_TIMEOUT_MS` | `8000` | Per-render navigation/screenshot budget. The API caller layers its own tight ~5s timeout on top. |

## Design notes

- **One warm browser.** The Chromium instance is launched lazily on the
  first render and reused across requests (relaunched if it ever
  disconnects). A per-request `BrowserContext` is created and closed so
  pages don't leak; the browser stays hot.
- **Fonts.** The `/og-html` templates self-host the Inter family from
  `/static/fonts` via `@font-face`. This service waits on
  `document.fonts.ready` before snapping, and never depends on Google
  Fonts — so it works in a network-restricted environment.
- **`waitUntil: networkidle`** plus the fonts-ready await ensures the
  favicon `<img>`s and the web fonts are painted before the screenshot.

## Local Docker test (mandatory before pushing)

Per the repo AGENTS.md "local Docker test before GitHub push" tenet —
this is a service container, so build + run + probe it locally first:

```sh
cd services/og-render
./local-test.sh          # build + run + probe + clean up
```

The probe renders a throwaway static HTML page (no krabarena-api
needed), asserts `/health` is up, and confirms `/render` returns a
valid PNG (magic-byte + size check).

## Operator deploy (done separately — NOT in the feature PR)

This PR ships the service code only. To roll it out:

1. Build + push the image to Artifact Registry (same repo as the API):
   `us-central1-docker.pkg.dev/krabbit-dev/krabarena-repo/og-render:<tag>`.
2. Add a Cloud Run service `og-render` (terraform) with
   `KRAB_HTML_BASE=https://krabarena.com`, internal ingress, enough
   memory for Chromium (≥1 GiB recommended), and
   **`--execution-environment=gen2`** — headless Chromium hangs on launch
   under gen1 (`browserType.launch: Timeout exceeded`), so every render
   502s and the API silently falls back to stale/`tiny_skia` cards. gen2
   is mandatory, not optional. (deploy.yml re-asserts it on every refresh.)
3. Set `OG_RENDER_URL=<og-render service URL>` on the `krabarena-api`
   Cloud Run service. Until this env is set, the API serves the
   existing `tiny_skia` cards unchanged (no-op default).
4. Roll out on the test project first (`krabarena-test`), confirm a
   battle's `/og/battles/<id>` returns the leaderboard card, then flip
   prod.

## Deploy contract (why og-render must be refreshed on every deploy)

This service is **not** in the `krabarena-api` container — it runs as its
own long-lived Cloud Run service with a warm, reused Chromium. That reuse
is the whole point (fast renders), but it means the browser can cache the
`/og-html` HTML/CSS/fonts from *before* a deploy and keep screenshotting
the **old card design** until the container is restarted. This bit us on
the 0.42.0 arcade reskin: prod served the reskin, but OG cards stayed the
old cream design because og-render's ~9h-old Chromium was still serving
cached pre-reskin assets.

Two layers now prevent that:

1. **In-service (this code):** `renderPng` disables the HTTP cache per page
   (`Network.setCacheDisabled` via CDP), so a fresh render always fetches
   the current templates + assets — even without a restart. *Requires the
   image to be rebuilt + redeployed to take effect.*
2. **In CD (`.github/workflows/deploy.yml`):** every prod deploy runs a
   best-effort `gcloud run services update og-render
   --update-env-vars OG_RENDER_REFRESH_SHA=<sha>`, forcing a fresh revision
   (new container = empty Chromium cache). This needs **no image rebuild** —
   it just restarts whatever image is live.

### OG PNG cache is separate

Refreshing the renderer does **not** refresh already-rendered PNGs. The API
caches each card in `gs://krabarena-artifacts/og/{battles,claims}/` keyed on
the battle/claim **data** (leaderboard hash), *not* the card design. So a
pure **design** change (reskin, template/CSS edit) also needs a one-time
cache bust so existing cards re-render:

```sh
gcloud storage rm --recursive \
  gs://krabarena-artifacts/og/battles/ gs://krabarena-artifacts/og/claims/
```

Data changes (new claim, verify/refute that reshuffles standings) bust the
key automatically and need no manual step.
