# OG-card render service & the `OG_RENDER_URL` flag

KrabArena's per-entity social cards (`/og/battles/{id}`,
`/og/claims/{id}`) can be rendered two ways:

1. **`tiny_skia` (default, always available).** A pure-Rust 2D
   rasteriser compiled into the API binary. Title + summary + a small
   logo strip. No external dependencies. This is what ships today and
   remains the fallback. See `crates/krabarena-api/src/services/og_image.rs`.
2. **Headless-Chromium leaderboard cards (opt-in).** A separate
   microservice (`services/og-render/`) screenshots an HTML template
   into a 1200×630 PNG. The card shows the actual battle/claim
   **leaderboard**, not just the title.

## How the dynamic path works

```
scraper ─► GET /og/battles/{id}          (krabarena-api)
              │
              ├─ OG_RENDER_URL unset ──► tiny_skia card (unchanged)
              │
              └─ OG_RENDER_URL set
                   │  GCS cache hit? ──► serve cached PNG
                   │
                   └─ miss ─► GET {OG_RENDER_URL}/render?path=/og-html/battles/{id}
                                 │   (og-render service, ~5s timeout)
                                 │
                                 ├─ 200 PNG ─► cache in GCS + serve
                                 └─ error/timeout ─► tiny_skia fallback
```

- **`/og-html/battles/{id}` / `/og-html/claims/{id}`** (askama,
  `crates/krabarena-api/src/routes/og_html.rs`) render the
  self-contained 1200×630 HTML the render service screenshots. They
  reuse `battle_summarizer::cumulative_wins_for_battle` (battles) and
  the claim's own `claim_structure.leaderboard[]` (claims). Internal-ish
  but harmless if public.
- The **GCS cache key** for a dynamic card folds the leaderboard
  standings into the hash, so a new claim / a verify-refute that
  reshuffles the board busts the cached PNG automatically.
- Fonts are **self-hosted** (Inter from `/static/fonts`) so the render
  service never needs Google Fonts — it can run network-restricted.

## Eager cache-warming (since 0.22.1)

By default the cache refreshes **lazily**: because the cache key folds
in the summary + leaderboard, the *next* scraper fetch after a change
is a miss → fresh render. That means a social card can lag behind the
latest standings until something re-scrapes the URL. Eager warming
closes that gap by proactively re-rendering + re-caching the card the
moment the content changes.

```
summary persisted ──► spawn (fire-and-forget)
                         │  OG_RENDER_URL set?  ── no ──► no-op
                         │            yes
                         └─ GET {OG_RENDER_URL}/render?path=/og-html/battles/{id}
                               └─ 200 PNG ─► upload to the SAME GCS key /og/battles/{id} reads
```

**1. Automatic warm on summary regeneration.** After a battle's
executive summary is persisted, the API fire-and-forgets a warm of the
battle's OG card. This fires on both:

- `POST /v1/battles/{id}/regenerate-summary` (the admin/author rebuild
  path), and
- the incremental summary refresh that runs after every new claim is
  published.

It is **best-effort**: spawned on a detached `tokio` task so it never
blocks the HTTP response, and a render-service hiccup or GCS write
failure is logged at `warn` and swallowed — summary regeneration can
never fail or slow down because of OG warming. When `OG_RENDER_URL` is
unset the warm is a cheap no-op (the task isn't even spawned).

**2. On-demand `krab battle regenerate-og <battle-id>`.** New
admin-only endpoint `POST /v1/battles/{battle_id}/regenerate-og`
re-warms the battle's card **and** every live (non-deleted,
non-superseded) claim's card under it, sequentially. Response:

```json
{ "enabled": true, "battle": true, "claims_warmed": 7, "failed": 0 }
```

On an instance where `OG_RENDER_URL` isn't set, it returns
`{ "enabled": false, "message": "...", ... }` rather than silently
doing nothing, so the operator knows the render service isn't wired up.

### Why the warm key must match the serve key

The warm path writes the PNG to GCS; the `/og/...` serve path reads it.
If the two computed different object names, the warm would be wasted —
the serve path would still miss and re-render. Both sides therefore
compute the key through a **single shared helper**
(`battle_cache_object_name` / `claim_cache_object_name` in
`routes/og.rs`), which folds the content hash + leaderboard hash into
the `og/<kind>/<id>-<hash>.png` layout exactly once. A unit test
(`claim_warm_and_serve_share_the_same_cache_key`) pins this so the two
paths can't silently drift.

## The `OG_RENDER_URL` rollout flag

`OG_RENDER_URL` is read per-request on the API.

- **Unset / empty (default): no-op.** The API serves the existing
  `tiny_skia` cards exactly as before. Shipping this feature changes
  nothing in production until the operator sets the env.
- **Set to the og-render service's base URL** (e.g.
  `https://og-render-xxxx-uc.a.run.app`): the API uses the dynamic
  leaderboard cards, with automatic fallback to `tiny_skia` on any
  render failure.

Roll out on `krabarena-test` first, confirm a battle's
`/og/battles/{id}` returns the leaderboard card, then set the env on
prod's `krabarena-api` Cloud Run service.

## Operator deploy steps (NOT done by the feature PR)

The feature PR ships code only. To enable:

1. **Build + push** the og-render image to Artifact Registry:
   `us-central1-docker.pkg.dev/krabbit-dev/krabarena-repo/og-render:<tag>`.
2. **Add a Cloud Run service** `og-render` (terraform) with
   `KRAB_HTML_BASE=https://krabarena.com`, internal ingress, and
   ≥1 GiB memory for Chromium.
3. **Set `OG_RENDER_URL`** = the og-render service URL on the
   `krabarena-api` Cloud Run service (both test and, after
   verification, prod).

See `services/og-render/README.md` for the service internals and the
local Docker test loop.
